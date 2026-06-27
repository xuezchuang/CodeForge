#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelResponseDecision {
    ExecuteToolCalls,
    RetryRequiredToolCall,
    RetryEmptyToolCall,
    RequestFinalWithoutTools,
    RequestBudgetFinal,
    Finalize,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelResponseDecisionInput<'a> {
    pub tool_call_count: usize,
    pub finish_reason: Option<&'a str>,
    pub round_index: usize,
    pub max_tool_rounds: usize,
    pub require_tool_call: bool,
    pub required_tool_call_response_retries: usize,
    pub empty_tool_call_response_retries: usize,
    pub empty_tool_call_response_retry_limit: usize,
}

pub fn decide_after_model_response(input: ModelResponseDecisionInput<'_>) -> ModelResponseDecision {
    if input.tool_call_count == 0 {
        if input.require_tool_call
            && input.round_index == 0
            && input.required_tool_call_response_retries == 0
            && input.finish_reason != Some("tool_calls")
        {
            return ModelResponseDecision::RetryRequiredToolCall;
        }

        if input.finish_reason == Some("tool_calls") {
            if input.empty_tool_call_response_retries < input.empty_tool_call_response_retry_limit
                && input.round_index < input.max_tool_rounds
            {
                return ModelResponseDecision::RetryEmptyToolCall;
            }
            return ModelResponseDecision::RequestFinalWithoutTools;
        }

        return ModelResponseDecision::Finalize;
    }

    if input.round_index >= input.max_tool_rounds {
        ModelResponseDecision::RequestBudgetFinal
    } else {
        ModelResponseDecision::ExecuteToolCalls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> ModelResponseDecisionInput<'static> {
        ModelResponseDecisionInput {
            tool_call_count: 0,
            finish_reason: Some("stop"),
            round_index: 0,
            max_tool_rounds: 32,
            require_tool_call: false,
            required_tool_call_response_retries: 0,
            empty_tool_call_response_retries: 0,
            empty_tool_call_response_retry_limit: 1,
        }
    }

    #[test]
    fn required_tool_call_missing_retries_once() {
        let decision = decide_after_model_response(ModelResponseDecisionInput {
            require_tool_call: true,
            ..input()
        });

        assert_eq!(decision, ModelResponseDecision::RetryRequiredToolCall);
    }

    #[test]
    fn empty_tool_call_finish_retries_then_falls_back() {
        let retry = decide_after_model_response(ModelResponseDecisionInput {
            finish_reason: Some("tool_calls"),
            ..input()
        });
        let fallback = decide_after_model_response(ModelResponseDecisionInput {
            finish_reason: Some("tool_calls"),
            empty_tool_call_response_retries: 1,
            ..input()
        });

        assert_eq!(retry, ModelResponseDecision::RetryEmptyToolCall);
        assert_eq!(fallback, ModelResponseDecision::RequestFinalWithoutTools);
    }

    #[test]
    fn tool_calls_execute_until_budget_is_reached() {
        let execute = decide_after_model_response(ModelResponseDecisionInput {
            tool_call_count: 1,
            round_index: 31,
            ..input()
        });
        let budget_final = decide_after_model_response(ModelResponseDecisionInput {
            tool_call_count: 1,
            round_index: 32,
            ..input()
        });

        assert_eq!(execute, ModelResponseDecision::ExecuteToolCalls);
        assert_eq!(budget_final, ModelResponseDecision::RequestBudgetFinal);
    }
}
