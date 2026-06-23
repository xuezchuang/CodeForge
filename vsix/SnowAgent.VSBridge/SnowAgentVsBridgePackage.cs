using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;
using EnvDTE;
using EnvDTE80;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Task = System.Threading.Tasks.Task;

namespace SnowAgent.VSBridge
{
    [PackageRegistration(UseManagedResourcesOnly = true, AllowsBackgroundLoading = true)]
    [InstalledProductRegistration("SnowAgent VS Bridge", "Registers this Visual Studio instance with SnowAgent Desktop.", "0.1.0")]
    [ProvideAutoLoad(VSConstants.UICONTEXT.ShellInitialized_string, PackageAutoLoadFlags.BackgroundLoad)]
    [ProvideAutoLoad(VSConstants.UICONTEXT.NoSolution_string, PackageAutoLoadFlags.BackgroundLoad)]
    [ProvideAutoLoad(VSConstants.UICONTEXT.SolutionExists_string, PackageAutoLoadFlags.BackgroundLoad)]
    [Guid(ExtensionInfo.PackageGuidString)]
    public sealed class SnowAgentVsBridgePackage : AsyncPackage
    {
        private DTE2 dte;
        private SolutionEvents solutionEvents;
        private BridgeHttpServer bridgeServer;
        private DesktopRegistrar desktopRegistrar;
        private Timer registrationTimer;
        private string lastRegisteredSolutionPath;
        private const int MaxDocumentChars = 200000;
        private const int MaxDocumentLines = 5000;
        private const int DefaultMaxProjectFiles = 2000;
        private const int MaxProjectFilesHardLimit = 10000;
        private const int DefaultMaxSearchResults = 100;
        private const int MaxSearchResultsHardLimit = 500;
        private const int DefaultSearchContextLines = 2;
        private const int MaxSearchContextLines = 200;
        private const int MaxSearchContentBytes = 2 * 1024 * 1024;
        private static readonly string[] IgnoredPathSegments =
        {
            "bin",
            "obj",
            ".vs",
            "Debug",
            "Release",
            "x64",
            ".git",
            "node_modules",
            "Intermediate",
            "Binaries",
            "Saved",
            "DerivedDataCache",
        };
        private static readonly string[] DefaultContentExtensions =
        {
            ".h",
            ".hpp",
            ".c",
            ".cpp",
            ".cc",
            ".cxx",
            ".inl",
            ".ixx",
            ".cs",
            ".sln",
            ".vcxproj",
            ".props",
            ".targets",
            ".json",
            ".xml",
            ".txt",
            ".md",
        };

        protected override async Task InitializeAsync(CancellationToken cancellationToken, IProgress<ServiceProgressData> progress)
        {
            ActivityLog.LogInformation("SnowAgent VS Bridge", "Package initialization started.");
            await JoinableTaskFactory.SwitchToMainThreadAsync(cancellationToken);

            var dteService = await GetServiceAsync(typeof(DTE)).ConfigureAwait(true);
            if (dteService == null)
            {
                ActivityLog.LogError("SnowAgent VS Bridge", "DTE service was not available.");
                return;
            }

            dte = dteService as DTE2;
            if (dte == null)
            {
                ActivityLog.LogError("SnowAgent VS Bridge", "DTE service did not resolve to DTE2.");
                return;
            }

            bridgeServer = new BridgeHttpServer(this);
            bridgeServer.Start();
            ActivityLog.LogInformation("SnowAgent VS Bridge", "Bridge server started at " + bridgeServer.Endpoint + ".");
            desktopRegistrar = new DesktopRegistrar();

            solutionEvents = dte.Events.SolutionEvents;
            solutionEvents.Opened += OnSolutionOpened;
            solutionEvents.AfterClosing += OnSolutionClosed;

            _ = JoinableTaskFactory.RunAsync(RegisterCurrentSolutionAsync);
            registrationTimer = new Timer(
                _ => _ = JoinableTaskFactory.RunAsync(RegisterCurrentSolutionAsync),
                null,
                TimeSpan.FromSeconds(5),
                TimeSpan.FromSeconds(10));
        }

        internal async Task<BridgeResponse> OpenFileAsync(OpenFileRequest request)
        {
            if (request == null || string.IsNullOrWhiteSpace(request.Path))
            {
                return new BridgeResponse { Ok = false, Message = "path is required" };
            }

            await JoinableTaskFactory.SwitchToMainThreadAsync();

            var fullPath = Path.GetFullPath(request.Path);
            if (!File.Exists(fullPath))
            {
                return new BridgeResponse { Ok = false, Message = "file does not exist: " + fullPath };
            }

            try
            {
                var window = dte.ItemOperations.OpenFile(fullPath);
                window?.Activate();

                var document = dte.ActiveDocument;
                document?.Activate();

                if (document?.Selection is TextSelection selection)
                {
                    var line = Math.Max(1, request.Line);
                    var column = Math.Max(1, request.Column ?? 1);
                    selection.MoveToLineAndOffset(line, column, false);
                }

                return new BridgeResponse { Ok = true, Message = "opened" };
            }
            catch (Exception ex)
            {
                return new BridgeResponse { Ok = false, Message = "openFile failed: " + ex.Message };
            }
        }

        internal async Task<CurrentSolutionResponse> CurrentSolutionAsync()
        {
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                var solutionPath = dte?.Solution?.FullName;
                if (string.IsNullOrWhiteSpace(solutionPath))
                {
                    return new CurrentSolutionResponse
                    {
                        Ok = true,
                        Message = "no_solution_open",
                        SolutionPath = null,
                        SolutionName = null,
                        IsOpen = false,
                    };
                }

                return new CurrentSolutionResponse
                {
                    Ok = true,
                    Message = "ok",
                    SolutionPath = Path.GetFullPath(solutionPath),
                    SolutionName = Path.GetFileNameWithoutExtension(solutionPath),
                    IsOpen = true,
                };
            }
            catch (Exception ex)
            {
                return new CurrentSolutionResponse
                {
                    Ok = false,
                    Message = "currentSolution failed: " + ex.Message,
                    SolutionPath = null,
                    SolutionName = null,
                    IsOpen = false,
                };
            }
        }

        internal async Task<CurrentDocumentResponse> CurrentDocumentAsync()
        {
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                var document = dte?.ActiveDocument;
                if (document == null)
                {
                    return new CurrentDocumentResponse
                    {
                        Ok = false,
                        Message = "no_active_document",
                        Text = string.Empty,
                    };
                }

                var textDocument = GetTextDocument(document);
                if (textDocument == null)
                {
                    return new CurrentDocumentResponse
                    {
                        Ok = false,
                        Message = "active_document_is_not_text",
                        Path = SafeDocumentPath(document),
                        Name = SafeDocumentName(document),
                        Language = LanguageFromPath(SafeDocumentPath(document)),
                        Text = string.Empty,
                    };
                }

                var text = ReadTextDocument(textDocument);
                var totalLines = Math.Max(1, SafeLine(textDocument.EndPoint));
                var truncated = false;
                text = TruncateText(text, MaxDocumentChars, MaxDocumentLines, out truncated);

                var selection = document.Selection as TextSelection;
                var activePoint = selection?.ActivePoint;

                return new CurrentDocumentResponse
                {
                    Ok = true,
                    Message = "ok",
                    Path = SafeDocumentPath(document),
                    Name = SafeDocumentName(document),
                    Language = LanguageFromPath(SafeDocumentPath(document)),
                    Line = SafeLine(activePoint),
                    Column = SafeColumn(activePoint),
                    Text = text,
                    TextTruncated = truncated,
                    TotalLines = totalLines,
                };
            }
            catch (Exception ex)
            {
                return new CurrentDocumentResponse
                {
                    Ok = false,
                    Message = "currentDocument failed: " + ex.Message,
                    Text = string.Empty,
                };
            }
        }

        internal async Task<CurrentSelectionResponse> CurrentSelectionAsync()
        {
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                var document = dte?.ActiveDocument;
                if (document == null)
                {
                    return new CurrentSelectionResponse
                    {
                        Ok = false,
                        Message = "no_active_document",
                        Text = string.Empty,
                        IsEmpty = true,
                    };
                }

                var selection = document.Selection as TextSelection;
                if (selection == null)
                {
                    return new CurrentSelectionResponse
                    {
                        Ok = false,
                        Message = "active_document_selection_is_not_text",
                        Path = SafeDocumentPath(document),
                        Text = string.Empty,
                        IsEmpty = true,
                    };
                }

                var activePoint = selection.ActivePoint;
                var anchorPoint = selection.AnchorPoint;
                var startPoint = ComesBeforeOrEqual(activePoint, anchorPoint) ? activePoint : anchorPoint;
                var endPoint = ComesBeforeOrEqual(activePoint, anchorPoint) ? anchorPoint : activePoint;
                string text;
                try
                {
                    text = selection.Text ?? string.Empty;
                }
                catch
                {
                    text = string.Empty;
                }

                return new CurrentSelectionResponse
                {
                    Ok = true,
                    Message = "ok",
                    Path = SafeDocumentPath(document),
                    StartLine = SafeLine(startPoint),
                    StartColumn = SafeColumn(startPoint),
                    EndLine = SafeLine(endPoint),
                    EndColumn = SafeColumn(endPoint),
                    Text = text,
                    IsEmpty = string.IsNullOrEmpty(text),
                };
            }
            catch (Exception ex)
            {
                return new CurrentSelectionResponse
                {
                    Ok = false,
                    Message = "currentSelection failed: " + ex.Message,
                    Text = string.Empty,
                    IsEmpty = true,
                };
            }
        }

        internal async Task<ProjectListResponse> ListProjectsAsync()
        {
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                var projects = new List<ProjectInfoDto>();
                CollectProjects(dte?.Solution?.Projects, projects);

                return new ProjectListResponse
                {
                    Ok = true,
                    Message = "ok",
                    Projects = projects.ToArray(),
                };
            }
            catch (Exception ex)
            {
                return new ProjectListResponse
                {
                    Ok = false,
                    Message = "listProjects failed: " + ex.Message,
                    Projects = new ProjectInfoDto[0],
                };
            }
        }

        internal async Task<ProjectFilesResponse> ListProjectFilesAsync(ProjectFilesRequest request)
        {
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                var maxFiles = NormalizeMaxFiles(request?.MaxFiles);
                bool truncated;
                string projectName;
                string errorMessage;
                var files = CollectProjectFilesForRequest(request, maxFiles, out truncated, out projectName, out errorMessage);
                if (errorMessage != null)
                {
                    return new ProjectFilesResponse
                    {
                        Ok = false,
                        Message = errorMessage,
                        ProjectName = projectName,
                        Files = new ProjectFileDto[0],
                        Truncated = false,
                    };
                }

                return new ProjectFilesResponse
                {
                    Ok = true,
                    Message = "ok",
                    ProjectName = projectName,
                    Files = files.ToArray(),
                    Truncated = truncated,
                };
            }
            catch (Exception ex)
            {
                return new ProjectFilesResponse
                {
                    Ok = false,
                    Message = "listProjectFiles failed: " + ex.Message,
                    ProjectName = ProjectFilterLabel(request),
                    Files = new ProjectFileDto[0],
                    Truncated = false,
                };
            }
        }

        internal async Task<SearchFilesResponse> SearchFilesAsync(SearchFilesRequest request)
        {
            var maxResults = NormalizeMaxSearchResults(request?.MaxResults);
            var pattern = request?.Pattern;
            if (string.IsNullOrWhiteSpace(pattern))
            {
                return SearchFilesError(request, "invalid_arguments: pattern must not be empty");
            }

            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                bool projectListTruncated;
                string projectName;
                string errorMessage;
                var files = CollectProjectFilesForRequest(
                    ProjectRequestFromSearch(request, MaxProjectFilesHardLimit),
                    MaxProjectFilesHardLimit,
                    out projectListTruncated,
                    out projectName,
                    out errorMessage);

                if (errorMessage != null)
                {
                    return SearchFilesError(request, errorMessage);
                }

                if (files.Count == 0)
                {
                    return SearchFilesError(request, "no_project_files");
                }

                var matches = new List<SearchFileMatchDto>();
                foreach (var file in files)
                {
                    var displayPath = DisplayPath(request.WorkspaceRoot, file.Path);
                    if (!IsUnderRequestedRoot(file.Path, displayPath, request.WorkspaceRoot, request.Root))
                    {
                        continue;
                    }

                    int score;
                    int[] indices;
                    if (!TryScoreFileMatch(pattern, displayPath, out score, out indices))
                    {
                        continue;
                    }

                    matches.Add(new SearchFileMatchDto
                    {
                        Path = displayPath,
                        Type = "file",
                        Score = score,
                        Indices = indices,
                    });
                }

                matches.Sort((left, right) =>
                {
                    var scoreOrder = right.Score.CompareTo(left.Score);
                    return scoreOrder != 0
                        ? scoreOrder
                        : string.Compare(left.Path, right.Path, StringComparison.OrdinalIgnoreCase);
                });

                var totalMatches = matches.Count;
                var shownCount = Math.Min(maxResults, totalMatches);
                var shownMatches = matches.GetRange(0, shownCount);
                var paths = new string[shownMatches.Count];
                for (var i = 0; i < shownMatches.Count; i++)
                {
                    paths[i] = shownMatches[i].Path;
                }

                var truncated = projectListTruncated || totalMatches > shownCount;
                return new SearchFilesResponse
                {
                    Ok = true,
                    Message = truncated
                        ? "too_many_results: returned first " + shownCount.ToString(CultureInfo.InvariantCulture) + " matches"
                        : "ok",
                    Root = NormalizeRootLabel(request.Root),
                    Pattern = pattern.Trim(),
                    Matches = shownMatches.ToArray(),
                    Paths = paths,
                    Count = shownCount,
                    TotalMatches = totalMatches,
                    Shown = shownCount,
                    Complete = !truncated,
                    MaxResults = maxResults,
                    ScannedFiles = files.Count,
                    Truncated = truncated,
                    Engine = "vsix-solution-file-search",
                    Source = "vsix",
                };
            }
            catch (Exception ex)
            {
                return SearchFilesError(request, "searchFiles failed: " + ex.Message);
            }
        }

        internal async Task<SearchContentResponse> SearchContentAsync(SearchContentRequest request)
        {
            var maxResults = NormalizeMaxSearchResults(request?.MaxResults);
            var contextLines = NormalizeContextLines(request?.ContextLines);
            var query = request?.Query;
            if (string.IsNullOrWhiteSpace(query))
            {
                return SearchContentError(request, "invalid_arguments: query must not be empty");
            }

            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                bool projectListTruncated;
                string projectName;
                string errorMessage;
                var files = CollectProjectFilesForRequest(
                    ProjectRequestFromSearch(request, MaxProjectFilesHardLimit),
                    MaxProjectFilesHardLimit,
                    out projectListTruncated,
                    out projectName,
                    out errorMessage);

                if (errorMessage != null)
                {
                    return SearchContentError(request, errorMessage);
                }

                if (files.Count == 0)
                {
                    return SearchContentError(request, "no_project_files");
                }

                return await Task.Run(() =>
                    SearchContentFiles(request, files, projectListTruncated, maxResults, contextLines))
                    .ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                return SearchContentError(request, "searchContent failed: " + ex.Message);
            }
        }

        internal async Task<ErrorListResponse> GetErrorListAsync()
        {
            try
            {
                await JoinableTaskFactory.SwitchToMainThreadAsync();

                return new ErrorListResponse
                {
                    Ok = true,
                    Message = "not_available",
                    Diagnostics = new ErrorDiagnosticDto[0],
                    Available = false,
                };
            }
            catch (Exception ex)
            {
                return new ErrorListResponse
                {
                    Ok = true,
                    Message = "not_available: " + ex.Message,
                    Diagnostics = new ErrorDiagnosticDto[0],
                    Available = false,
                };
            }
        }

        private void OnSolutionOpened()
        {
            ActivityLog.LogInformation("SnowAgent VS Bridge", "Solution opened event received.");
            _ = JoinableTaskFactory.RunAsync(RegisterCurrentSolutionAsync);
        }

        private void OnSolutionClosed()
        {
            ActivityLog.LogInformation("SnowAgent VS Bridge", "Solution closed event received.");
            lastRegisteredSolutionPath = null;
            var registrar = desktopRegistrar;
            if (registrar != null)
            {
                _ = Task.Run(() => registrar.UnregisterAsync());
            }
        }

        private async Task RegisterCurrentSolutionAsync()
        {
            await JoinableTaskFactory.SwitchToMainThreadAsync();

            var solutionPath = dte?.Solution?.FullName;
            if (string.IsNullOrWhiteSpace(solutionPath) || bridgeServer == null)
            {
                solutionPath = TryGetSolutionPathFromCommandLine();
                if (string.IsNullOrWhiteSpace(solutionPath) || bridgeServer == null)
                {
                    ActivityLog.LogInformation("SnowAgent VS Bridge", "Registration skipped because no solution is open yet.");
                    return;
                }

                ActivityLog.LogInformation(
                    "SnowAgent VS Bridge",
                    "Using command line solution path for registration: " + solutionPath + ".");
            }

            if (string.Equals(lastRegisteredSolutionPath, solutionPath, StringComparison.OrdinalIgnoreCase))
            {
                return;
            }

            var processId = System.Diagnostics.Process.GetCurrentProcess().Id;
            var payload = new VsRegisterPayload
            {
                InstanceId = "vs-" + processId.ToString(CultureInfo.InvariantCulture),
                ProcessId = processId,
                SolutionPath = solutionPath,
                Endpoint = bridgeServer.Endpoint,
            };

            try
            {
                await desktopRegistrar.RegisterAsync(payload).ConfigureAwait(false);
                lastRegisteredSolutionPath = solutionPath;
                ActivityLog.LogInformation(
                    "SnowAgent VS Bridge",
                    "Registered " + payload.InstanceId + " for " + solutionPath + " at " + payload.Endpoint + ".");
            }
            catch (Exception ex)
            {
                // SnowAgent Desktop may not be running yet. Opening/reloading the solution retries registration.
                ActivityLog.LogWarning(
                    "SnowAgent VS Bridge",
                    "Registration failed for " + solutionPath + ": " + ex.Message);
            }
        }

        private static string TryGetSolutionPathFromCommandLine()
        {
            var commandLine = Environment.CommandLine;
            if (string.IsNullOrWhiteSpace(commandLine))
            {
                ActivityLog.LogInformation("SnowAgent VS Bridge", "Command line was empty.");
                return null;
            }

            ActivityLog.LogInformation("SnowAgent VS Bridge", "Command line is: " + commandLine);
            var match = Regex.Match(commandLine, @"(?<path>[A-Za-z]:\\.*?\.sln)", RegexOptions.IgnoreCase);
            if (!match.Success)
            {
                ActivityLog.LogInformation("SnowAgent VS Bridge", "No solution path was found in the command line.");
                return null;
            }

            var path = match.Groups["path"].Value.Trim().Trim('"');
            if (!File.Exists(path))
            {
                ActivityLog.LogInformation("SnowAgent VS Bridge", "Command line solution path does not exist: " + path);
                return null;
            }

            return Path.GetFullPath(path);
        }

        private static TextDocument GetTextDocument(Document document)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (document == null)
            {
                return null;
            }

            try
            {
                return document.Object("TextDocument") as TextDocument;
            }
            catch
            {
                return null;
            }
        }

        private static string ReadTextDocument(TextDocument textDocument)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (textDocument == null)
            {
                return string.Empty;
            }

            var startPoint = textDocument.StartPoint.CreateEditPoint();
            return startPoint.GetText(textDocument.EndPoint) ?? string.Empty;
        }

        private static string TruncateText(string text, int maxChars, int maxLines, out bool truncated)
        {
            text = text ?? string.Empty;
            truncated = false;

            var lineLimitIndex = IndexAfterLineLimit(text, maxLines);
            if (lineLimitIndex >= 0 && lineLimitIndex < text.Length)
            {
                text = text.Substring(0, lineLimitIndex);
                truncated = true;
            }

            if (text.Length > maxChars)
            {
                text = text.Substring(0, maxChars);
                truncated = true;
            }

            return text;
        }

        private static int IndexAfterLineLimit(string text, int maxLines)
        {
            if (maxLines <= 0)
            {
                return 0;
            }

            var lineCount = 1;
            for (var index = 0; index < text.Length; index++)
            {
                if (text[index] != '\n')
                {
                    continue;
                }

                lineCount++;
                if (lineCount > maxLines)
                {
                    return index + 1;
                }
            }

            return -1;
        }

        private static string SafeDocumentPath(Document document)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return string.IsNullOrWhiteSpace(document?.FullName)
                    ? null
                    : Path.GetFullPath(document.FullName);
            }
            catch
            {
                return null;
            }
        }

        private static string SafeDocumentName(Document document)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return document?.Name;
            }
            catch
            {
                return null;
            }
        }

        private static int SafeLine(TextPoint point)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return Math.Max(1, point?.Line ?? 1);
            }
            catch
            {
                return 1;
            }
        }

        private static int SafeColumn(TextPoint point)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return Math.Max(1, point?.LineCharOffset ?? 1);
            }
            catch
            {
                return 1;
            }
        }

        private static bool ComesBeforeOrEqual(TextPoint left, TextPoint right)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            var leftLine = SafeLine(left);
            var rightLine = SafeLine(right);
            if (leftLine != rightLine)
            {
                return leftLine < rightLine;
            }

            return SafeColumn(left) <= SafeColumn(right);
        }

        private static string LanguageFromPath(string path)
        {
            var extension = Path.GetExtension(path ?? string.Empty).ToLowerInvariant();
            switch (extension)
            {
                case ".cpp":
                case ".cc":
                case ".cxx":
                case ".c":
                    return "cpp";
                case ".h":
                case ".hpp":
                case ".hh":
                case ".hxx":
                    return "cpp-header";
                case ".cs":
                    return "csharp";
                case ".ts":
                    return "typescript";
                case ".tsx":
                    return "typescriptreact";
                case ".rs":
                    return "rust";
                case ".json":
                    return "json";
                case ".xml":
                    return "xml";
                case ".sln":
                    return "solution";
                case ".vcxproj":
                    return "vcxproj";
                default:
                    return string.IsNullOrEmpty(extension) ? "unknown" : extension.TrimStart('.');
            }
        }

        private static void CollectProjects(Projects projects, List<ProjectInfoDto> results)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (projects == null)
            {
                return;
            }

            foreach (Project project in projects)
            {
                CollectProject(project, results);
            }
        }

        private static void CollectProject(Project project, List<ProjectInfoDto> results)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (project == null)
            {
                return;
            }

            results.Add(new ProjectInfoDto
            {
                Name = SafeProjectName(project),
                FullName = SafeProjectFullName(project),
                Kind = SafeProjectKind(project),
                UniqueName = SafeProjectUniqueName(project),
            });

            CollectSubProjects(SafeProjectItems(project), results);
        }

        private static void CollectSubProjects(ProjectItems items, List<ProjectInfoDto> results)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (items == null)
            {
                return;
            }

            foreach (ProjectItem item in items)
            {
                Project subProject = null;
                try
                {
                    subProject = item.SubProject;
                }
                catch
                {
                    subProject = null;
                }

                if (subProject != null)
                {
                    CollectProject(subProject, results);
                }
            }
        }

        private static void CollectProjectObjects(Projects projects, List<Project> results)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (projects == null)
            {
                return;
            }

            foreach (Project project in projects)
            {
                CollectProjectObject(project, results);
            }
        }

        private static void CollectProjectObject(Project project, List<Project> results)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (project == null)
            {
                return;
            }

            results.Add(project);
            var items = SafeProjectItems(project);
            if (items == null)
            {
                return;
            }

            foreach (ProjectItem item in items)
            {
                try
                {
                    if (item.SubProject != null)
                    {
                        CollectProjectObject(item.SubProject, results);
                    }
                }
                catch
                {
                    // Ignore individual solution folder entries that cannot be inspected.
                }
            }
        }

        private List<ProjectFileDto> CollectProjectFilesForRequest(
            ProjectFilesRequest request,
            int maxFiles,
            out bool truncated,
            out string projectName,
            out string errorMessage)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            projectName = HasProjectFilter(request) ? ProjectFilterLabel(request) : "all";
            errorMessage = null;
            truncated = false;

            var allProjects = new List<Project>();
            CollectProjectObjects(dte?.Solution?.Projects, allProjects);
            var selectedProjects = SelectProjects(allProjects, request);
            if (selectedProjects.Count == 0 && HasProjectFilter(request))
            {
                errorMessage = "project_not_found";
                return new List<ProjectFileDto>();
            }

            var files = new List<ProjectFileDto>();
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var project in selectedProjects)
            {
                CollectProjectItemFiles(SafeProjectItems(project), files, seen, maxFiles, ref truncated);
                if (truncated)
                {
                    break;
                }
            }

            return files;
        }

        private static List<Project> SelectProjects(List<Project> projects, ProjectFilesRequest request)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (!HasProjectFilter(request))
            {
                return projects;
            }

            var projectName = request.ProjectName?.Trim();
            var uniqueName = request.ProjectUniqueName?.Trim();
            var selected = new List<Project>();
            foreach (var project in projects)
            {
                if (!string.IsNullOrWhiteSpace(projectName) &&
                    string.Equals(SafeProjectName(project), projectName, StringComparison.OrdinalIgnoreCase))
                {
                    selected.Add(project);
                    continue;
                }

                if (!string.IsNullOrWhiteSpace(uniqueName) &&
                    string.Equals(SafeProjectUniqueName(project), uniqueName, StringComparison.OrdinalIgnoreCase))
                {
                    selected.Add(project);
                }
            }

            return selected;
        }

        private static bool HasProjectFilter(ProjectFilesRequest request)
        {
            return !string.IsNullOrWhiteSpace(request?.ProjectName) ||
                !string.IsNullOrWhiteSpace(request?.ProjectUniqueName);
        }

        private static string ProjectFilterLabel(ProjectFilesRequest request)
        {
            if (!string.IsNullOrWhiteSpace(request?.ProjectUniqueName))
            {
                return request.ProjectUniqueName.Trim();
            }

            if (!string.IsNullOrWhiteSpace(request?.ProjectName))
            {
                return request.ProjectName.Trim();
            }

            return null;
        }

        private static int NormalizeMaxFiles(int? maxFiles)
        {
            var value = maxFiles.GetValueOrDefault(DefaultMaxProjectFiles);
            if (value <= 0)
            {
                return DefaultMaxProjectFiles;
            }

            return Math.Min(value, MaxProjectFilesHardLimit);
        }

        private static int NormalizeMaxSearchResults(int? maxResults)
        {
            var value = maxResults.GetValueOrDefault(DefaultMaxSearchResults);
            if (value <= 0)
            {
                return DefaultMaxSearchResults;
            }

            return Math.Min(value, MaxSearchResultsHardLimit);
        }

        private static int NormalizeContextLines(int? contextLines)
        {
            var value = contextLines.GetValueOrDefault(DefaultSearchContextLines);
            if (value < 0)
            {
                return DefaultSearchContextLines;
            }

            return Math.Min(value, MaxSearchContextLines);
        }

        private static ProjectFilesRequest ProjectRequestFromSearch(SearchFilesRequest request, int maxFiles)
        {
            return new ProjectFilesRequest
            {
                ProjectName = request?.ProjectName,
                ProjectUniqueName = request?.ProjectUniqueName,
                MaxFiles = maxFiles,
            };
        }

        private static ProjectFilesRequest ProjectRequestFromSearch(SearchContentRequest request, int maxFiles)
        {
            return new ProjectFilesRequest
            {
                ProjectName = request?.ProjectName,
                ProjectUniqueName = request?.ProjectUniqueName,
                MaxFiles = maxFiles,
            };
        }

        private static SearchFilesResponse SearchFilesError(SearchFilesRequest request, string message)
        {
            return new SearchFilesResponse
            {
                Ok = false,
                Message = message,
                Root = NormalizeRootLabel(request?.Root),
                Pattern = request?.Pattern,
                Matches = new SearchFileMatchDto[0],
                Paths = new string[0],
                Count = 0,
                TotalMatches = 0,
                Shown = 0,
                Complete = false,
                MaxResults = NormalizeMaxSearchResults(request?.MaxResults),
                ScannedFiles = 0,
                Truncated = false,
                Engine = "vsix-solution-file-search",
                Source = "vsix",
            };
        }

        private static SearchContentResponse SearchContentError(SearchContentRequest request, string message)
        {
            return new SearchContentResponse
            {
                Ok = false,
                Message = message,
                Query = request?.Query,
                Root = NormalizeRootLabel(request?.Root),
                FileGlob = request?.FileGlob,
                MaxResults = NormalizeMaxSearchResults(request?.MaxResults),
                ContextLines = NormalizeContextLines(request?.ContextLines),
                CaseSensitive = request?.CaseSensitive.GetValueOrDefault(false) ?? false,
                Regex = request?.Regex.GetValueOrDefault(false) ?? false,
                Engine = "vsix-solution-content-search",
                Source = "vsix",
                Matches = new SearchContentMatchDto[0],
                Count = 0,
                ScannedFiles = 0,
                Complete = false,
                Truncated = false,
            };
        }

        private static SearchContentResponse SearchContentFiles(
            SearchContentRequest request,
            List<ProjectFileDto> files,
            bool projectListTruncated,
            int maxResults,
            int contextLines)
        {
            var query = request.Query;
            var useRegex = request.Regex.GetValueOrDefault(false);
            var caseSensitive = request.CaseSensitive.GetValueOrDefault(false);
            Regex compiledRegex = null;
            if (useRegex)
            {
                try
                {
                    compiledRegex = new Regex(
                        query,
                        caseSensitive ? RegexOptions.None : RegexOptions.IgnoreCase);
                }
                catch (Exception ex)
                {
                    return SearchContentError(request, "invalid_regex: " + ex.Message);
                }
            }

            var matches = new List<SearchContentMatchDto>();
            var scannedFiles = 0;
            var resultTruncated = false;
            foreach (var file in files)
            {
                if (matches.Count >= maxResults)
                {
                    resultTruncated = true;
                    break;
                }

                var displayPath = DisplayPath(request.WorkspaceRoot, file.Path);
                if (!IsUnderRequestedRoot(file.Path, displayPath, request.WorkspaceRoot, request.Root) ||
                    !ContentFileAllowed(displayPath, file.Path, request.FileGlob))
                {
                    continue;
                }

                var lines = ReadTextLinesBestEffort(file.Path);
                if (lines == null)
                {
                    continue;
                }

                scannedFiles++;
                for (var index = 0; index < lines.Length; index++)
                {
                    if (matches.Count >= maxResults)
                    {
                        resultTruncated = true;
                        break;
                    }

                    var columns = FindMatchColumns(lines[index], query, caseSensitive, compiledRegex);
                    if (columns.Length == 0)
                    {
                        continue;
                    }

                    var lineNumber = index + 1;
                    matches.Add(new SearchContentMatchDto
                    {
                        File = displayPath,
                        Line = lineNumber,
                        Column = columns[0],
                        Columns = columns,
                        Text = lines[index],
                        Before = BuildContextLines(lines, Math.Max(1, lineNumber - contextLines), lineNumber - 1),
                        After = BuildContextLines(lines, lineNumber + 1, Math.Min(lines.Length, lineNumber + contextLines)),
                    });
                }
            }

            var truncated = projectListTruncated || resultTruncated;
            return new SearchContentResponse
            {
                Ok = true,
                Message = truncated
                    ? "too_many_results: returned first " + matches.Count.ToString(CultureInfo.InvariantCulture) + " matches"
                    : "ok",
                Query = query,
                Root = NormalizeRootLabel(request.Root),
                FileGlob = request.FileGlob,
                MaxResults = maxResults,
                ContextLines = contextLines,
                CaseSensitive = caseSensitive,
                Regex = useRegex,
                Engine = "vsix-solution-content-search",
                Source = "vsix",
                Matches = matches.ToArray(),
                Count = matches.Count,
                ScannedFiles = scannedFiles,
                Complete = !truncated,
                Truncated = truncated,
            };
        }

        private static bool TryScoreFileMatch(string pattern, string displayPath, out int score, out int[] indices)
        {
            score = 0;
            indices = new int[0];

            var normalizedPath = NormalizePathSeparators(displayPath);
            var fileName = DisplayFileName(normalizedPath);
            var patternText = NormalizePathSeparators(pattern).Trim();
            if (string.IsNullOrWhiteSpace(patternText))
            {
                return false;
            }

            var pathLower = normalizedPath.ToLowerInvariant();
            var fileNameLower = fileName.ToLowerInvariant();
            var patternLower = patternText.ToLowerInvariant();
            var fileNameStart = Math.Max(0, normalizedPath.Length - fileName.Length);

            if (fileNameLower == patternLower)
            {
                score = 10000 - normalizedPath.Length;
                indices = RangeIndices(fileNameStart, fileName.Length);
                return true;
            }

            if (pathLower == patternLower)
            {
                score = 9500 - normalizedPath.Length;
                indices = RangeIndices(0, normalizedPath.Length);
                return true;
            }

            var fileNameIndex = fileNameLower.IndexOf(patternLower, StringComparison.Ordinal);
            if (fileNameIndex >= 0)
            {
                score = 8000 - (fileNameIndex * 10) - normalizedPath.Length;
                indices = RangeIndices(fileNameStart + fileNameIndex, patternText.Length);
                return true;
            }

            var pathIndex = pathLower.IndexOf(patternLower, StringComparison.Ordinal);
            if (pathIndex >= 0)
            {
                score = 6500 - (pathIndex * 10) - normalizedPath.Length;
                indices = RangeIndices(pathIndex, patternText.Length);
                return true;
            }

            var literalPattern = patternLower.Replace("*", string.Empty).Replace("?", string.Empty);
            if ((patternLower.IndexOf('*') >= 0 || patternLower.IndexOf('?') >= 0) &&
                !string.IsNullOrWhiteSpace(literalPattern) &&
                (WildcardMatch(fileNameLower, patternLower) || WildcardMatch(pathLower, patternLower)))
            {
                var literalIndex = pathLower.IndexOf(literalPattern, StringComparison.Ordinal);
                score = 5200 - normalizedPath.Length;
                indices = literalIndex >= 0
                    ? RangeIndices(literalIndex, literalPattern.Length)
                    : new int[0];
                return true;
            }

            int[] fuzzyIndices;
            if (TryOrderedMatch(pathLower, literalPattern, out fuzzyIndices))
            {
                var spread = fuzzyIndices.Length == 0
                    ? 0
                    : fuzzyIndices[fuzzyIndices.Length - 1] - fuzzyIndices[0];
                score = 3500 - (spread * 5) - normalizedPath.Length;
                indices = fuzzyIndices;
                return true;
            }

            return false;
        }

        private static bool TryOrderedMatch(string text, string pattern, out int[] indices)
        {
            var results = new List<int>();
            indices = new int[0];
            if (string.IsNullOrWhiteSpace(pattern))
            {
                return false;
            }

            var searchStart = 0;
            foreach (var character in pattern)
            {
                if (char.IsWhiteSpace(character) || character == '*' || character == '?')
                {
                    continue;
                }

                var found = text.IndexOf(character.ToString(), searchStart, StringComparison.Ordinal);
                if (found < 0)
                {
                    return false;
                }

                results.Add(found);
                searchStart = found + 1;
            }

            if (results.Count == 0)
            {
                return false;
            }

            indices = results.ToArray();
            return true;
        }

        private static int[] FindMatchColumns(string line, string query, bool caseSensitive, Regex regex)
        {
            if (regex != null)
            {
                var regexColumns = new List<int>();
                foreach (Match match in regex.Matches(line ?? string.Empty))
                {
                    if (match.Success)
                    {
                        regexColumns.Add(match.Index + 1);
                    }
                }

                return regexColumns.ToArray();
            }

            var columns = new List<int>();
            var comparison = caseSensitive ? StringComparison.Ordinal : StringComparison.OrdinalIgnoreCase;
            var offset = 0;
            var text = line ?? string.Empty;
            while (offset <= text.Length)
            {
                var found = text.IndexOf(query, offset, comparison);
                if (found < 0)
                {
                    break;
                }

                columns.Add(found + 1);
                offset = found + Math.Max(1, query.Length);
            }

            return columns.ToArray();
        }

        private static SearchContentLineDto[] BuildContextLines(string[] lines, int startLine, int endLine)
        {
            if (lines == null || startLine > endLine)
            {
                return new SearchContentLineDto[0];
            }

            var start = Math.Max(1, startLine);
            var end = Math.Min(lines.Length, endLine);
            var results = new List<SearchContentLineDto>();
            for (var lineNumber = start; lineNumber <= end; lineNumber++)
            {
                results.Add(new SearchContentLineDto
                {
                    Line = lineNumber,
                    Text = lines[lineNumber - 1],
                });
            }

            return results.ToArray();
        }

        private static string[] ReadTextLinesBestEffort(string path)
        {
            try
            {
                var info = new FileInfo(path);
                if (!info.Exists || info.Length > MaxSearchContentBytes)
                {
                    return null;
                }

                var bytes = File.ReadAllBytes(path);
                if (Array.IndexOf(bytes, (byte)0) >= 0)
                {
                    return null;
                }

                string text;
                try
                {
                    text = new UTF8Encoding(false, true).GetString(bytes);
                }
                catch
                {
                    text = Encoding.Default.GetString(bytes);
                }

                text = text.Replace("\r\n", "\n").Replace('\r', '\n');
                return text.Split('\n');
            }
            catch
            {
                return null;
            }
        }

        private static bool ContentFileAllowed(string displayPath, string fullPath, string fileGlob)
        {
            if (!string.IsNullOrWhiteSpace(fileGlob))
            {
                var normalizedPath = NormalizePathSeparators(displayPath).ToLowerInvariant();
                var fileName = DisplayFileName(normalizedPath);
                var pattern = NormalizePathSeparators(fileGlob).Trim().ToLowerInvariant();
                return WildcardMatch(normalizedPath, pattern) || WildcardMatch(fileName, pattern);
            }

            var extension = Path.GetExtension(fullPath);
            if (string.IsNullOrWhiteSpace(extension))
            {
                return false;
            }

            foreach (var allowedExtension in DefaultContentExtensions)
            {
                if (string.Equals(extension, allowedExtension, StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }

            return false;
        }

        private static bool IsUnderRequestedRoot(string fullPath, string displayPath, string workspaceRoot, string root)
        {
            if (string.IsNullOrWhiteSpace(root) || root.Trim() == ".")
            {
                return true;
            }

            var rootPath = TryResolveRootPath(workspaceRoot, root);
            var filePath = TryGetFullPath(fullPath);
            if (!string.IsNullOrWhiteSpace(rootPath) &&
                !string.IsNullOrWhiteSpace(filePath) &&
                IsSameOrUnderPath(rootPath, filePath))
            {
                return true;
            }

            var normalizedRoot = NormalizePathSeparators(root).Trim().Trim('/');
            var normalizedDisplay = NormalizePathSeparators(displayPath).TrimStart('/');
            return string.Equals(normalizedDisplay, normalizedRoot, StringComparison.OrdinalIgnoreCase) ||
                normalizedDisplay.StartsWith(normalizedRoot + "/", StringComparison.OrdinalIgnoreCase);
        }

        private static string TryResolveRootPath(string workspaceRoot, string root)
        {
            if (string.IsNullOrWhiteSpace(root))
            {
                return null;
            }

            try
            {
                if (Path.IsPathRooted(root))
                {
                    return Path.GetFullPath(root);
                }

                if (!string.IsNullOrWhiteSpace(workspaceRoot))
                {
                    return Path.GetFullPath(Path.Combine(workspaceRoot, root));
                }
            }
            catch
            {
                return null;
            }

            return null;
        }

        private static string DisplayPath(string workspaceRoot, string fullPath)
        {
            var filePath = TryGetFullPath(fullPath);
            if (string.IsNullOrWhiteSpace(filePath))
            {
                return NormalizePathSeparators(fullPath);
            }

            var rootPath = TryGetFullPath(workspaceRoot);
            if (!string.IsNullOrWhiteSpace(rootPath) && IsSameOrUnderPath(rootPath, filePath))
            {
                var relativePath = MakeRelativePath(rootPath, filePath);
                return NormalizePathSeparators(relativePath);
            }

            return NormalizePathSeparators(filePath);
        }

        private static string TryGetFullPath(string path)
        {
            if (string.IsNullOrWhiteSpace(path))
            {
                return null;
            }

            try
            {
                return Path.GetFullPath(path);
            }
            catch
            {
                return null;
            }
        }

        private static bool IsSameOrUnderPath(string rootPath, string candidatePath)
        {
            if (string.Equals(rootPath, candidatePath, StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }

            var rootWithSeparator = EnsureTrailingDirectorySeparator(rootPath);
            return candidatePath.StartsWith(rootWithSeparator, StringComparison.OrdinalIgnoreCase);
        }

        private static string MakeRelativePath(string rootPath, string filePath)
        {
            if (string.Equals(rootPath, filePath, StringComparison.OrdinalIgnoreCase))
            {
                return ".";
            }

            try
            {
                var rootUri = new Uri(EnsureTrailingDirectorySeparator(rootPath));
                var fileUri = new Uri(filePath);
                if (rootUri.Scheme != fileUri.Scheme)
                {
                    return filePath;
                }

                return Uri.UnescapeDataString(rootUri.MakeRelativeUri(fileUri).ToString())
                    .Replace('/', Path.DirectorySeparatorChar);
            }
            catch
            {
                return filePath;
            }
        }

        private static string EnsureTrailingDirectorySeparator(string path)
        {
            if (string.IsNullOrEmpty(path) ||
                path.EndsWith(Path.DirectorySeparatorChar.ToString(), StringComparison.Ordinal) ||
                path.EndsWith(Path.AltDirectorySeparatorChar.ToString(), StringComparison.Ordinal))
            {
                return path;
            }

            return path + Path.DirectorySeparatorChar;
        }

        private static string NormalizeRootLabel(string root)
        {
            if (string.IsNullOrWhiteSpace(root))
            {
                return ".";
            }

            var normalized = NormalizePathSeparators(root).Trim().Trim('/');
            return string.IsNullOrWhiteSpace(normalized) ? "." : normalized;
        }

        private static string NormalizePathSeparators(string path)
        {
            return (path ?? string.Empty).Replace('\\', '/');
        }

        private static string DisplayFileName(string displayPath)
        {
            if (string.IsNullOrWhiteSpace(displayPath))
            {
                return string.Empty;
            }

            var normalized = NormalizePathSeparators(displayPath);
            var separator = normalized.LastIndexOf('/');
            return separator >= 0 ? normalized.Substring(separator + 1) : normalized;
        }

        private static int[] RangeIndices(int start, int length)
        {
            if (start < 0 || length <= 0)
            {
                return new int[0];
            }

            var indices = new int[length];
            for (var index = 0; index < length; index++)
            {
                indices[index] = start + index;
            }

            return indices;
        }

        private static bool WildcardMatch(string text, string pattern)
        {
            var textIndex = 0;
            var patternIndex = 0;
            var starIndex = -1;
            var starTextIndex = 0;

            while (textIndex < text.Length)
            {
                if (patternIndex < pattern.Length &&
                    (pattern[patternIndex] == '?' || pattern[patternIndex] == text[textIndex]))
                {
                    textIndex++;
                    patternIndex++;
                }
                else if (patternIndex < pattern.Length && pattern[patternIndex] == '*')
                {
                    starIndex = patternIndex;
                    starTextIndex = textIndex;
                    patternIndex++;
                }
                else if (starIndex >= 0)
                {
                    patternIndex = starIndex + 1;
                    starTextIndex++;
                    textIndex = starTextIndex;
                }
                else
                {
                    return false;
                }
            }

            while (patternIndex < pattern.Length && pattern[patternIndex] == '*')
            {
                patternIndex++;
            }

            return patternIndex == pattern.Length;
        }

        private static void CollectProjectItemFiles(
            ProjectItems items,
            List<ProjectFileDto> files,
            HashSet<string> seen,
            int maxFiles,
            ref bool truncated)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            if (items == null || truncated)
            {
                return;
            }

            foreach (ProjectItem item in items)
            {
                if (files.Count >= maxFiles)
                {
                    truncated = true;
                    return;
                }

                AddProjectItemFiles(item, files, seen, maxFiles, ref truncated);
                if (truncated)
                {
                    return;
                }

                Project subProject = null;
                try
                {
                    subProject = item.SubProject;
                }
                catch
                {
                    subProject = null;
                }

                if (subProject != null)
                {
                    CollectProjectItemFiles(SafeProjectItems(subProject), files, seen, maxFiles, ref truncated);
                }

                CollectProjectItemFiles(SafeChildItems(item), files, seen, maxFiles, ref truncated);
            }
        }

        private static void AddProjectItemFiles(
            ProjectItem item,
            List<ProjectFileDto> files,
            HashSet<string> seen,
            int maxFiles,
            ref bool truncated)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            int fileCount;
            try
            {
                fileCount = item.FileCount;
            }
            catch
            {
                return;
            }

            for (var index = 1; index <= fileCount; index++)
            {
                if (files.Count >= maxFiles)
                {
                    truncated = true;
                    return;
                }

                string path;
                try
                {
                    path = item.FileNames[(short)index];
                }
                catch
                {
                    continue;
                }

                AddFilePath(path, files, seen);
            }
        }

        private static void AddFilePath(string path, List<ProjectFileDto> files, HashSet<string> seen)
        {
            if (string.IsNullOrWhiteSpace(path))
            {
                return;
            }

            string fullPath;
            try
            {
                fullPath = Path.GetFullPath(path);
            }
            catch
            {
                return;
            }

            if (!File.Exists(fullPath) || IsIgnoredPath(fullPath) || !seen.Add(fullPath))
            {
                return;
            }

            files.Add(new ProjectFileDto
            {
                Path = fullPath,
                Name = Path.GetFileName(fullPath),
            });
        }

        private static bool IsIgnoredPath(string path)
        {
            var normalized = path.Replace('/', '\\');
            foreach (var segment in IgnoredPathSegments)
            {
                if (normalized.IndexOf("\\" + segment + "\\", StringComparison.OrdinalIgnoreCase) >= 0)
                {
                    return true;
                }
            }

            return false;
        }

        private static ProjectItems SafeProjectItems(Project project)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return project?.ProjectItems;
            }
            catch
            {
                return null;
            }
        }

        private static ProjectItems SafeChildItems(ProjectItem item)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return item?.ProjectItems;
            }
            catch
            {
                return null;
            }
        }

        private static string SafeProjectName(Project project)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return project?.Name;
            }
            catch
            {
                return null;
            }
        }

        private static string SafeProjectFullName(Project project)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                var fullName = project?.FullName;
                return string.IsNullOrWhiteSpace(fullName) ? null : Path.GetFullPath(fullName);
            }
            catch
            {
                return null;
            }
        }

        private static string SafeProjectKind(Project project)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return project?.Kind;
            }
            catch
            {
                return null;
            }
        }

        private static string SafeProjectUniqueName(Project project)
        {
            ThreadHelper.ThrowIfNotOnUIThread();

            try
            {
                return project?.UniqueName;
            }
            catch
            {
                return null;
            }
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                var events = solutionEvents;
                var registrar = desktopRegistrar;
                registrationTimer?.Dispose();

                JoinableTaskFactory.Run(async () =>
                {
                    await JoinableTaskFactory.SwitchToMainThreadAsync();

                    if (events != null)
                    {
                        events.Opened -= OnSolutionOpened;
                        events.AfterClosing -= OnSolutionClosed;
                    }

                    if (registrar != null)
                    {
                        await registrar.UnregisterAsync().ConfigureAwait(false);
                    }
                });

                desktopRegistrar?.Dispose();
                bridgeServer?.Dispose();

                solutionEvents = null;
                desktopRegistrar = null;
                bridgeServer = null;
            }

            base.Dispose(disposing);
        }
    }
}
