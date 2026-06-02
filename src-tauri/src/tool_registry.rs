use serde_json::{json, Value};

pub const CALCULATOR_ADD_TOOL_NAME: &str = "calculator.add";

pub fn tool_definitions() -> Vec<Value> {
    vec![json!({
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
    })]
}

pub fn execute_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        CALCULATOR_ADD_TOOL_NAME => add(arguments),
        _ => Err(format!("Unknown tool: {name}")),
    }
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

    #[test]
    fn calculator_add_returns_sum() {
        let result = execute_tool(CALCULATOR_ADD_TOOL_NAME, &json!({ "a": 1, "b": 1 })).unwrap();

        assert_eq!(result, json!({ "result": 2 }));
    }

    #[test]
    fn calculator_add_requires_numbers() {
        let error =
            execute_tool(CALCULATOR_ADD_TOOL_NAME, &json!({ "a": "1", "b": 1 })).unwrap_err();

        assert!(error.contains("numeric field `a`"));
    }
}
