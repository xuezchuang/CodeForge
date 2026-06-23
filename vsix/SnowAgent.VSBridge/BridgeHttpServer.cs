using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace SnowAgent.VSBridge
{
    internal sealed class BridgeHttpServer : IDisposable
    {
        private readonly SnowAgentVsBridgePackage package;
        private readonly CancellationTokenSource cancellation = new CancellationTokenSource();
        private TcpListener listener;
        private Task acceptLoop;

        public BridgeHttpServer(SnowAgentVsBridgePackage package)
        {
            this.package = package;
        }

        public int Port { get; private set; }

        public string Endpoint => "http://127.0.0.1:" + Port;

        public void Start()
        {
            listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            Port = ((IPEndPoint)listener.LocalEndpoint).Port;
            acceptLoop = Task.Run(() => AcceptLoopAsync(cancellation.Token));
        }

        private async Task AcceptLoopAsync(CancellationToken token)
        {
            while (!token.IsCancellationRequested)
            {
                TcpClient client = null;
                try
                {
                    client = await listener.AcceptTcpClientAsync().ConfigureAwait(false);
                    _ = Task.Run(() => HandleClientAsync(client, token), token);
                }
                catch
                {
                    client?.Dispose();
                    if (!token.IsCancellationRequested)
                    {
                        await Task.Delay(250, token).ConfigureAwait(false);
                    }
                }
            }
        }

        private async Task HandleClientAsync(TcpClient client, CancellationToken token)
        {
            using (client)
            using (var stream = client.GetStream())
            {
                var request = await ReadRequestAsync(stream, token).ConfigureAwait(false);
                var requestLine = request.RequestLine;
                if (string.IsNullOrWhiteSpace(requestLine))
                {
                    return;
                }

                BridgeResponse response;
                if (IsPost(requestLine, "/openFile"))
                {
                    response = await HandleOpenFileAsync(request.Body).ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/currentSolution"))
                {
                    response = await package.CurrentSolutionAsync().ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/currentDocument"))
                {
                    response = await package.CurrentDocumentAsync().ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/currentSelection"))
                {
                    response = await package.CurrentSelectionAsync().ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/listProjects"))
                {
                    response = await package.ListProjectsAsync().ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/listProjectFiles"))
                {
                    response = await HandleListProjectFilesAsync(request.Body).ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/searchFiles"))
                {
                    response = await HandleSearchFilesAsync(request.Body).ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/searchContent"))
                {
                    response = await HandleSearchContentAsync(request.Body).ConfigureAwait(false);
                }
                else if (IsPost(requestLine, "/getErrorList"))
                {
                    response = await package.GetErrorListAsync().ConfigureAwait(false);
                }
                else
                {
                    response = new BridgeResponse { Ok = false, Message = "unsupported endpoint" };
                }

                await WriteResponseAsync(stream, response, token).ConfigureAwait(false);
            }
        }

        private static bool IsPost(string requestLine, string path)
        {
            return requestLine.StartsWith("POST " + path + " ", StringComparison.OrdinalIgnoreCase);
        }

        private static async Task<HttpRequestData> ReadRequestAsync(NetworkStream stream, CancellationToken token)
        {
            var headerBytes = new List<byte>();
            var oneByte = new byte[1];

            while (true)
            {
                var count = await stream.ReadAsync(oneByte, 0, oneByte.Length, token).ConfigureAwait(false);
                if (count == 0)
                {
                    break;
                }

                headerBytes.Add(oneByte[0]);
                var length = headerBytes.Count;
                if (length >= 4 &&
                    headerBytes[length - 4] == '\r' &&
                    headerBytes[length - 3] == '\n' &&
                    headerBytes[length - 2] == '\r' &&
                    headerBytes[length - 1] == '\n')
                {
                    break;
                }

                if (headerBytes.Count > 64 * 1024)
                {
                    throw new InvalidOperationException("HTTP header too large");
                }
            }

            var headerText = Encoding.ASCII.GetString(headerBytes.ToArray());
            var lines = headerText.Split(new[] { "\r\n" }, StringSplitOptions.None);
            var requestLine = lines.Length > 0 ? lines[0] : string.Empty;
            var contentLength = 0;

            foreach (var line in lines)
            {
                var separator = line.IndexOf(':');
                if (separator <= 0)
                {
                    continue;
                }

                var name = line.Substring(0, separator).Trim();
                var value = line.Substring(separator + 1).Trim();
                if (name.Equals("Content-Length", StringComparison.OrdinalIgnoreCase))
                {
                    int.TryParse(value, out contentLength);
                }
            }

            var bodyBytes = new byte[contentLength];
            var read = 0;
            while (read < contentLength)
            {
                var count = await stream.ReadAsync(bodyBytes, read, contentLength - read, token).ConfigureAwait(false);
                if (count == 0)
                {
                    break;
                }
                read += count;
            }

            return new HttpRequestData
            {
                RequestLine = requestLine,
                Body = Encoding.UTF8.GetString(bodyBytes, 0, read),
            };
        }

        private async Task<BridgeResponse> HandleOpenFileAsync(string body)
        {
            try
            {
                var request = JsonUtil.Deserialize<OpenFileRequest>(body);
                return await package.OpenFileAsync(request).ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                return new BridgeResponse { Ok = false, Message = ex.Message };
            }
        }

        private async Task<BridgeResponse> HandleListProjectFilesAsync(string body)
        {
            try
            {
                var request = string.IsNullOrWhiteSpace(body)
                    ? new ProjectFilesRequest()
                    : JsonUtil.Deserialize<ProjectFilesRequest>(body);
                return await package.ListProjectFilesAsync(request).ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                return new ProjectFilesResponse
                {
                    Ok = false,
                    Message = ex.Message,
                    ProjectName = null,
                    Files = new ProjectFileDto[0],
                    Truncated = false,
                };
            }
        }

        private async Task<BridgeResponse> HandleSearchFilesAsync(string body)
        {
            try
            {
                var request = string.IsNullOrWhiteSpace(body)
                    ? new SearchFilesRequest()
                    : JsonUtil.Deserialize<SearchFilesRequest>(body);
                return await package.SearchFilesAsync(request).ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                return new SearchFilesResponse
                {
                    Ok = false,
                    Message = ex.Message,
                    Root = ".",
                    Pattern = null,
                    Matches = new SearchFileMatchDto[0],
                    Paths = new string[0],
                    Count = 0,
                    TotalMatches = 0,
                    Shown = 0,
                    Complete = false,
                    MaxResults = 0,
                    ScannedFiles = 0,
                    Truncated = false,
                    Engine = "vsix-solution-file-search",
                    Source = "vsix",
                };
            }
        }

        private async Task<BridgeResponse> HandleSearchContentAsync(string body)
        {
            try
            {
                var request = string.IsNullOrWhiteSpace(body)
                    ? new SearchContentRequest()
                    : JsonUtil.Deserialize<SearchContentRequest>(body);
                return await package.SearchContentAsync(request).ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                return new SearchContentResponse
                {
                    Ok = false,
                    Message = ex.Message,
                    Query = null,
                    Root = ".",
                    FileGlob = null,
                    MaxResults = 0,
                    ContextLines = 0,
                    CaseSensitive = false,
                    Regex = false,
                    Engine = "vsix-solution-content-search",
                    Source = "vsix",
                    Matches = new SearchContentMatchDto[0],
                    Count = 0,
                    ScannedFiles = 0,
                    Complete = false,
                    Truncated = false,
                };
            }
        }

        private static async Task WriteResponseAsync(Stream stream, BridgeResponse response, CancellationToken token)
        {
            var status = response.Ok ? "200 OK" : "400 Bad Request";
            var json = JsonUtil.Serialize(response, response.GetType());
            var body = Encoding.UTF8.GetBytes(json);
            var header = Encoding.ASCII.GetBytes(
                "HTTP/1.1 " + status + "\r\n" +
                "Content-Type: application/json; charset=utf-8\r\n" +
                "Content-Length: " + body.Length + "\r\n" +
                "Connection: close\r\n\r\n");
            await stream.WriteAsync(header, 0, header.Length, token).ConfigureAwait(false);
            await stream.WriteAsync(body, 0, body.Length, token).ConfigureAwait(false);
        }

        public void Dispose()
        {
            cancellation.Cancel();
            listener?.Stop();
            try
            {
                acceptLoop?.Wait(TimeSpan.FromSeconds(1));
            }
            catch
            {
                // Ignore shutdown races.
            }
            cancellation.Dispose();
        }

        private sealed class HttpRequestData
        {
            public string RequestLine { get; set; }

            public string Body { get; set; }
        }
    }
}
