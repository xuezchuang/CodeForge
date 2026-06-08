use serde_json::{json, Value};

use crate::vs_bridge_client;
use crate::workspace_tools;

pub const CALCULATOR_ADD_TOOL_NAME: &str = "calculator.add";
pub const LIST_DIR_TOOL_NAME: &str = "list_dir";
pub const READ_FILE_TOOL_NAME: &str = "read_file";
pub const SEARCH_FILE_TOOL_NAME: &str = "search_file";
pub const SEARCH_CONTENT_TOOL_NAME: &str = "search_content";
pub const GET_FILE_CONTEXT_TOOL_NAME: &str = "get_file_context";
pub const VS_CURRENT_SOLUTION_TOOL_NAME: &str = "vs.current_solution";
pub const VS_CURRENT_DOCUMENT_TOOL_NAME: &str = "vs.current_document";
pub const VS_CURRENT_SELECTION_TOOL_NAME: &str = "vs.current_selection";
pub const VS_LIST_PROJECTS_TOOL_NAME: &str = "vs.list_projects";
pub const VS_LIST_PROJECT_FILES_TOOL_NAME: &str = "vs.list_project_files";
pub const VS_GET_ERROR_LIST_TOOL_NAME: &str = "vs.get_error_list";

pub struct ToolExecutionContext<'a> {
    pub workspace_root: &'a str,
    pub vs_bridge_endpoint: Option<&'a str>,
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        calculator_add_definition(),
        list_dir_definition(),
        read_file_definition(),
        search_file_definition(),
        search_content_definition(),
        get_file_context_definition(),
        vs_current_solution_definition(),
        vs_current_document_definition(),
        vs_current_selection_definition(),
        vs_list_projects_definition(),
        vs_list_project_files_definition(),
        vs_get_error_list_definition(),
    ]
}

pub async fn execute_tool(
    context: &ToolExecutionContext<'_>,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    match name {
        CALCULATOR_ADD_TOOL_NAME => add(arguments),
        LIST_DIR_TOOL_NAME => workspace_tools::list_dir(context.workspace_root, arguments),
        READ_FILE_TOOL_NAME => workspace_tools::read_file(context.workspace_root, arguments),
        SEARCH_FILE_TOOL_NAME => workspace_tools::search_file(context.workspace_root, arguments),
        SEARCH_CONTENT_TOOL_NAME => {
            workspace_tools::search_content(context.workspace_root, arguments)
        }
        GET_FILE_CONTEXT_TOOL_NAME => {
            workspace_tools::get_file_context(context.workspace_root, arguments)
        }
        VS_CURRENT_SOLUTION_TOOL_NAME => {
            vs_bridge_client::call_vs_current_solution(context.vs_bridge_endpoint).await
        }
        VS_CURRENT_DOCUMENT_TOOL_NAME => {
            vs_bridge_client::call_vs_current_document(context.vs_bridge_endpoint).await
        }
        VS_CURRENT_SELECTION_TOOL_NAME => {
            vs_bridge_client::call_vs_current_selection(context.vs_bridge_endpoint).await
        }
        VS_LIST_PROJECTS_TOOL_NAME => {
            vs_bridge_client::call_vs_list_projects(context.vs_bridge_endpoint).await
        }
        VS_LIST_PROJECT_FILES_TOOL_NAME => {
            vs_bridge_client::call_vs_list_project_files(context.vs_bridge_endpoint, arguments)
                .await
        }
        VS_GET_ERROR_LIST_TOOL_NAME => {
            vs_bridge_client::call_vs_get_error_list(context.vs_bridge_endpoint).await
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn calculator_add_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": CALCULATOR_ADD_TOOL_NAME,
            "description": "Add two numbers and return the result.",
            "parameters": {
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"]
            }
        }
    })
}

fn list_dir_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": LIST_DIR_TOOL_NAME,
            "description": "List immediate child directories and files under a workspace-relative path. Paths cannot escape the workspace root. Ignored directories include .git, .vs, bin, obj, build, out, node_modules, and .cache.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative directory path, for example . or src."
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn read_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_FILE_TOOL_NAME,
            "description": "Read a text file inside the workspace with line numbers. Defaults to at most 300 lines; use start_line and end_line for large files. Binary files are rejected.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative file path."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn search_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SEARCH_FILE_TOOL_NAME,
            "description": "Search for files by fuzzy filename or path inside the workspace. Results are ranked by exact filename, filename contains, fuzzy filename, then path matches.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Filename or path pattern to search for."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional workspace-relative directory to search under."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    }
                },
                "required": ["pattern"]
            }
        }
    })
}

fn search_content_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SEARCH_CONTENT_TOOL_NAME,
            "description": "Search text content inside workspace files with bounded traversal. Returns structured matches with file, line, column, text, before, and after. Narrow root or file_glob for large repositories.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text or regex to search for."
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional workspace-relative directory to search under."
                    },
                    "file_glob": {
                        "type": "string",
                        "description": "Optional glob such as *.cpp, **/*.h, or *.rs."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 100
                    },
                    "context_lines": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 2
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "default": false
                    },
                    "regex": {
                        "type": "boolean",
                        "default": false
                    }
                },
                "required": ["query"]
            }
        }
    })
}

fn get_file_context_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": GET_FILE_CONTEXT_TOOL_NAME,
            "description": "Read line-numbered context around one line in a workspace file. Defaults to 30 lines before and after.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative file path."
                    },
                    "line": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "before": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 30
                    },
                    "after": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 30
                    }
                },
                "required": ["path", "line"]
            }
        }
    })
}

fn vs_current_solution_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_CURRENT_SOLUTION_TOOL_NAME,
            "description": "Read the current Visual Studio solution through the connected VS Bridge. Requires Bridge Connected; returns bridge_not_connected when Visual Studio is not connected.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_current_document_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_CURRENT_DOCUMENT_TOOL_NAME,
            "description": "Read the active Visual Studio text document through the connected VS Bridge. Returns path, cursor line/column, language, text, totalLines, and textTruncated. Requires Bridge Connected; returned text may be truncated.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_current_selection_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_CURRENT_SELECTION_TOOL_NAME,
            "description": "Read the active Visual Studio text selection through the connected VS Bridge. Returns current selection text and start/end line/column, or isEmpty=true for an empty caret selection. Requires Bridge Connected.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_list_projects_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_LIST_PROJECTS_TOOL_NAME,
            "description": "List projects currently loaded in the active Visual Studio solution through the connected VS Bridge. Handles solution folders best-effort. Requires Bridge Connected.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn vs_list_project_files_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_LIST_PROJECT_FILES_TOOL_NAME,
            "description": "Lightweight DTE ProjectItems file enumeration through the connected VS Bridge. This is not a full code graph or semantic index. Requires Bridge Connected and returns truncated=true if the file limit is hit.",
            "parameters": {
                "type": "object",
                "properties": {
                    "projectName": {
                        "type": "string",
                        "description": "Optional Visual Studio project display name to enumerate. If omitted, all loaded projects are scanned."
                    },
                    "projectUniqueName": {
                        "type": "string",
                        "description": "Optional Visual Studio project UniqueName to enumerate."
                    },
                    "maxFiles": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 2000,
                        "description": "Maximum files to return before truncating."
                    }
                }
            }
        }
    })
}

fn vs_get_error_list_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VS_GET_ERROR_LIST_TOOL_NAME,
            "description": "Read Visual Studio Error List diagnostics through the connected VS Bridge when available. The current VSIX may return available=false and message=not_available; do not treat this as clangd, LSP, or full code graph analysis.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        }
    })
}

fn add(arguments: &Value) -> Result<Value, String> {
    let a = read_number(arguments, "a")?;
    let b = read_number(arguments, "b")?;
    Ok(json!({ "result": number_value(a + b) }))
}

fn read_number(arguments: &Value, key: &str) -> Result<f64, String> {
    arguments
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("calculator.add requires numeric field `{key}`"))
}

fn number_value(number: f64) -> Value {
    if number.is_finite()
        && number.fract() == 0.0
        && number <= i64::MAX as f64
        && number >= i64::MIN as f64
    {
        json!(number as i64)
    } else {
        json!(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> ToolExecutionContext<'static> {
        ToolExecutionContext {
            workspace_root: ".",
            vs_bridge_endpoint: None,
        }
    }

    #[test]
    fn calculator_add_returns_sum() {
        let result = tauri::async_runtime::block_on(execute_tool(
            &test_context(),
            CALCULATOR_ADD_TOOL_NAME,
            &json!({ "a": 1, "b": 1 }),
        ))
        .unwrap();

        assert_eq!(result, json!({ "result": 2 }));
    }

    #[test]
    fn calculator_add_requires_numbers() {
        let error = tauri::async_runtime::block_on(execute_tool(
            &test_context(),
            CALCULATOR_ADD_TOOL_NAME,
            &json!({ "a": "1", "b": 1 }),
        ))
        .unwrap_err();

        assert!(error.contains("numeric field `a`"));
    }

    #[test]
    fn unknown_tool_returns_error() {
        let error = tauri::async_runtime::block_on(execute_tool(
            &test_context(),
            "missing.tool",
            &json!({}),
        ))
        .unwrap_err();

        assert!(error.contains("Unknown tool: missing.tool"));
    }

    #[test]
    fn tool_definitions_include_workspace_search_tools() {
        let names = tool_definitions()
            .into_iter()
            .filter_map(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        assert!(names.contains(&LIST_DIR_TOOL_NAME.to_string()));
        assert!(names.contains(&READ_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&SEARCH_FILE_TOOL_NAME.to_string()));
        assert!(names.contains(&SEARCH_CONTENT_TOOL_NAME.to_string()));
        assert!(names.contains(&GET_FILE_CONTEXT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_SOLUTION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_DOCUMENT_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_CURRENT_SELECTION_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_LIST_PROJECTS_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_LIST_PROJECT_FILES_TOOL_NAME.to_string()));
        assert!(names.contains(&VS_GET_ERROR_LIST_TOOL_NAME.to_string()));
    }

    #[test]
    fn vs_tool_returns_bridge_not_connected_when_endpoint_missing() {
        let result = tauri::async_runtime::block_on(execute_tool(
            &test_context(),
            VS_CURRENT_DOCUMENT_TOOL_NAME,
            &json!({}),
        ))
        .unwrap();

        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["status"], json!("bridge_not_connected"));
        assert_eq!(result["source"], json!("vsix"));
    }
}
