using System.Runtime.Serialization;

namespace SnowAgent.VSBridge
{
    [DataContract]
    internal sealed class ProjectFilesRequest
    {
        [DataMember(Name = "projectName")]
        public string ProjectName { get; set; }

        [DataMember(Name = "projectUniqueName")]
        public string ProjectUniqueName { get; set; }

        [DataMember(Name = "maxFiles")]
        public int? MaxFiles { get; set; }
    }

    [DataContract]
    internal sealed class OpenFileRequest
    {
        [DataMember(Name = "path")]
        public string Path { get; set; }

        [DataMember(Name = "line")]
        public int Line { get; set; }

        [DataMember(Name = "column")]
        public int? Column { get; set; }
    }

    [DataContract]
    internal class BridgeResponse
    {
        [DataMember(Name = "ok")]
        public bool Ok { get; set; }

        [DataMember(Name = "message")]
        public string Message { get; set; }
    }

    [DataContract]
    internal sealed class CurrentSolutionResponse : BridgeResponse
    {
        [DataMember(Name = "solutionPath")]
        public string SolutionPath { get; set; }

        [DataMember(Name = "solutionName")]
        public string SolutionName { get; set; }

        [DataMember(Name = "isOpen")]
        public bool IsOpen { get; set; }
    }

    [DataContract]
    internal sealed class CurrentDocumentResponse : BridgeResponse
    {
        [DataMember(Name = "path")]
        public string Path { get; set; }

        [DataMember(Name = "name")]
        public string Name { get; set; }

        [DataMember(Name = "language")]
        public string Language { get; set; }

        [DataMember(Name = "line")]
        public int Line { get; set; }

        [DataMember(Name = "column")]
        public int Column { get; set; }

        [DataMember(Name = "text")]
        public string Text { get; set; }

        [DataMember(Name = "textTruncated")]
        public bool TextTruncated { get; set; }

        [DataMember(Name = "totalLines")]
        public int TotalLines { get; set; }
    }

    [DataContract]
    internal sealed class CurrentSelectionResponse : BridgeResponse
    {
        [DataMember(Name = "path")]
        public string Path { get; set; }

        [DataMember(Name = "startLine")]
        public int StartLine { get; set; }

        [DataMember(Name = "startColumn")]
        public int StartColumn { get; set; }

        [DataMember(Name = "endLine")]
        public int EndLine { get; set; }

        [DataMember(Name = "endColumn")]
        public int EndColumn { get; set; }

        [DataMember(Name = "text")]
        public string Text { get; set; }

        [DataMember(Name = "isEmpty")]
        public bool IsEmpty { get; set; }
    }

    [DataContract]
    internal sealed class ProjectListResponse : BridgeResponse
    {
        [DataMember(Name = "projects")]
        public ProjectInfoDto[] Projects { get; set; }
    }

    [DataContract]
    internal sealed class ProjectInfoDto
    {
        [DataMember(Name = "name")]
        public string Name { get; set; }

        [DataMember(Name = "fullName")]
        public string FullName { get; set; }

        [DataMember(Name = "kind")]
        public string Kind { get; set; }

        [DataMember(Name = "uniqueName")]
        public string UniqueName { get; set; }
    }

    [DataContract]
    internal sealed class ProjectFilesResponse : BridgeResponse
    {
        [DataMember(Name = "projectName")]
        public string ProjectName { get; set; }

        [DataMember(Name = "files")]
        public ProjectFileDto[] Files { get; set; }

        [DataMember(Name = "truncated")]
        public bool Truncated { get; set; }
    }

    [DataContract]
    internal sealed class ProjectFileDto
    {
        [DataMember(Name = "path")]
        public string Path { get; set; }

        [DataMember(Name = "name")]
        public string Name { get; set; }
    }

    [DataContract]
    internal sealed class ErrorListResponse : BridgeResponse
    {
        [DataMember(Name = "diagnostics")]
        public ErrorDiagnosticDto[] Diagnostics { get; set; }

        [DataMember(Name = "available")]
        public bool Available { get; set; }
    }

    [DataContract]
    internal sealed class ErrorDiagnosticDto
    {
        [DataMember(Name = "file")]
        public string File { get; set; }

        [DataMember(Name = "line")]
        public int Line { get; set; }

        [DataMember(Name = "column")]
        public int Column { get; set; }

        [DataMember(Name = "code")]
        public string Code { get; set; }

        [DataMember(Name = "message")]
        public string Message { get; set; }

        [DataMember(Name = "severity")]
        public string Severity { get; set; }

        [DataMember(Name = "project")]
        public string Project { get; set; }
    }

    [DataContract]
    internal sealed class VsRegisterPayload
    {
        [DataMember(Name = "instanceId")]
        public string InstanceId { get; set; }

        [DataMember(Name = "processId")]
        public int ProcessId { get; set; }

        [DataMember(Name = "solutionPath")]
        public string SolutionPath { get; set; }

        [DataMember(Name = "endpoint")]
        public string Endpoint { get; set; }
    }

    [DataContract]
    internal sealed class HeartbeatPayload
    {
        [DataMember(Name = "instanceId")]
        public string InstanceId { get; set; }
    }
}
