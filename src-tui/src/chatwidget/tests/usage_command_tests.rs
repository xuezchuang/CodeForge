use super::*;
use codex_protocol::num_format::format_with_separators;

fn last_token_usage_with(input: i64, cached: i64, output: i64, reasoning: i64) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        output_tokens: output,
        reasoning_output_tokens: reasoning,
        total_tokens: input + output,
    }
}

#[tokio::test]
async fn usage_command_prints_two_lines_with_last_and_total() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let info = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 12_000,
            cached_input_tokens: 8_000,
            output_tokens: 600,
            reasoning_output_tokens: 50,
            total_tokens: 12_600,
        },
        last_token_usage: TokenUsage {
            input_tokens: 4_000,
            cached_input_tokens: 2_500,
            output_tokens: 250,
            reasoning_output_tokens: 25,
            total_tokens: 4_250,
        },
        model_context_window: Some(128_000),
    };
    handle_token_count(&mut chat, Some(info));

    chat.dispatch_command(SlashCommand::Usage);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 120))
        }
        other => panic!("expected usage output, got {other:?}"),
    };

    assert!(
        rendered.contains("Token usage (this turn)"),
        "missing 'this turn' line, got: {rendered}"
    );
    assert!(
        rendered.contains("Token usage (session  )"),
        "missing 'session' line, got: {rendered}"
    );
    // Non-cached input for last = 4_000 - 2_500 = 1_500
    assert!(
        rendered.contains(&format!("input={}", format_with_separators(1_500))),
        "expected non-cached input 1,500 in this-turn line, got: {rendered}"
    );
    // Cached portion for last = 2_500
    assert!(
        rendered.contains(&format!("(+ {} cached)", format_with_separators(2_500))),
        "expected cached 2,500 in this-turn line, got: {rendered}"
    );
    // Non-cached input for total = 12_000 - 8_000 = 4_000
    assert!(
        rendered.contains(&format!("input={}", format_with_separators(4_000))),
        "expected non-cached input 4,000 in session line, got: {rendered}"
    );
}

#[tokio::test]
async fn usage_command_prints_placeholder_when_no_usage_recorded() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    // No token usage has been emitted.
    chat.dispatch_command(SlashCommand::Usage);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected usage output, got {other:?}"),
    };

    assert!(
        rendered.contains("(no usage recorded yet)"),
        "expected placeholder for empty usage, got: {rendered}"
    );
    // Both rows should still appear.
    assert!(rendered.contains("this turn"));
    assert!(rendered.contains("session"));
    // Silence unused-helper lint.
    let _ = last_token_usage_with(0, 0, 0, 0);
}
