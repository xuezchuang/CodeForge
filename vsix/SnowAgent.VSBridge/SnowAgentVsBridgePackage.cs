using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
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
                var allProjects = new List<Project>();
                CollectProjectObjects(dte?.Solution?.Projects, allProjects);
                var selectedProjects = SelectProjects(allProjects, request);
                if (selectedProjects.Count == 0 && HasProjectFilter(request))
                {
                    return new ProjectFilesResponse
                    {
                        Ok = false,
                        Message = "project_not_found",
                        ProjectName = ProjectFilterLabel(request),
                        Files = new ProjectFileDto[0],
                        Truncated = false,
                    };
                }

                var files = new List<ProjectFileDto>();
                var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
                var truncated = false;
                foreach (var project in selectedProjects)
                {
                    CollectProjectItemFiles(SafeProjectItems(project), files, seen, maxFiles, ref truncated);
                    if (truncated)
                    {
                        break;
                    }
                }

                return new ProjectFilesResponse
                {
                    Ok = true,
                    Message = "ok",
                    ProjectName = HasProjectFilter(request) ? ProjectFilterLabel(request) : "all",
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
