use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crossterm::cursor;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const PROMPT: &str = "› ";
const COMPOSER_BG: &str = "\x1b[48;5;236m";

#[derive(Parser, Debug)]
#[command(version, about = "CodeForge TUI shell")]
struct Cli {
    /// Optional prompt to submit immediately.
    prompt: Option<String>,

    /// Disable alternate screen mode. Kept for Codex CLI argument compatibility.
    #[arg(long = "no-alt-screen", default_value_t = false)]
    no_alt_screen: bool,

    /// Model label to show in the TUI.
    #[arg(short = 'm', long = "model", default_value = "MiniMax-M3")]
    model: String,
}

#[derive(Debug)]
struct Session {
    model: String,
    cwd: String,
    history: Vec<String>,
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste, ResetColor, cursor::Show);
        let _ = disable_raw_mode();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if !io::stdin().is_terminal() {
        return Ok(());
    }

    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .display()
        .to_string();
    let mut session = Session {
        model: cli.model,
        cwd,
        history: Vec::new(),
    };

    run_shell(&mut session, cli.prompt)?;
    Ok(())
}

fn run_shell(session: &mut Session, initial_prompt: Option<String>) -> Result<()> {
    enable_raw_mode()?;
    let _raw = RawModeGuard;
    execute!(io::stdout(), EnableBracketedPaste, cursor::Show)?;

    let mut input = initial_prompt.unwrap_or_default();
    let mut cursor_index = input.chars().count();
    let start_row = current_row_after_prompt()?;
    let mut first_frame = true;

    loop {
        render_frame(session, &input, cursor_index, start_row, first_frame)?;
        first_frame = false;

        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter => {
                        let submitted = input.trim().to_string();
                        if submitted.is_empty() {
                            continue;
                        }
                        finalize_submitted(start_row, session, &submitted)?;
                        session.history.push(submitted.clone());
                        render_working_line()?;
                        let reply = stub_backend_reply(&submitted);
                        clear_working_line()?;
                        print_assistant_reply(&reply)?;
                        session.history.push(reply);
                        input.clear();
                        cursor_index = 0;
                    }
                    KeyCode::Backspace => {
                        if cursor_index > 0 {
                            remove_char_at(&mut input, cursor_index - 1);
                            cursor_index -= 1;
                        }
                    }
                    KeyCode::Left => cursor_index = cursor_index.saturating_sub(1),
                    KeyCode::Right => {
                        cursor_index = (cursor_index + 1).min(input.chars().count());
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        clear_runtime_area(start_row)?;
                        println!();
                        io::stdout().flush()?;
                        return Ok(());
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        insert_char_at(&mut input, cursor_index, ch);
                        cursor_index += 1;
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                first_frame = true;
            }
            _ => {}
        }
    }
}

fn current_row_after_prompt() -> Result<u16> {
    let mut stdout = io::stdout();
    let (col, mut row) = cursor::position()?;
    if col > 0 {
        writeln!(stdout)?;
        stdout.flush()?;
        row = cursor::position()?.1;
    }
    Ok(row)
}

fn render_frame(
    session: &Session,
    input: &str,
    cursor_index: usize,
    start_row: u16,
    redraw_header: bool,
) -> Result<()> {
    let mut stdout = io::stdout();
    let (cols, rows) = crossterm::terminal::size()?;
    let footer_row = rows.saturating_sub(2);
    let composer_row = footer_row.saturating_sub(3);

    if redraw_header {
        clear_from(start_row)?;
        render_header(&mut stdout, session, start_row, cols)?;
        render_tip(&mut stdout, start_row + 7)?;
    } else {
        for row in composer_row..rows {
            queue_clear_line(&mut stdout, row)?;
        }
    }

    render_composer(&mut stdout, composer_row, cols, input, cursor_index)?;
    render_footer(&mut stdout, footer_row, cols, session)?;
    stdout.flush()?;
    Ok(())
}

fn render_header(stdout: &mut io::Stdout, session: &Session, row: u16, cols: u16) -> Result<()> {
    let title = "›_ CodeForge Codex (v0.1.0)";
    let model_line = format!("model:     {}   /model to change", session.model);
    let directory_line = format!("directory: {}", session.cwd);
    let width = [title.width(), model_line.width(), directory_line.width()]
        .into_iter()
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .max(43)
        .min(cols.saturating_sub(2) as usize);
    let horizontal = "─".repeat(width);
    write_header_line(stdout, row, &format!("╭{horizontal}╮"))?;
    write_header_content(stdout, row + 1, title, width)?;
    write_header_content(stdout, row + 2, "", width)?;
    write_header_content(stdout, row + 3, &model_line, width)?;
    write_header_content(stdout, row + 4, &directory_line, width)?;
    write_header_line(stdout, row + 5, &format!("╰{horizontal}╯"))?;
    Ok(())
}

fn render_tip(stdout: &mut io::Stdout, row: u16) -> Result<()> {
    queue_clear_line(stdout, row)?;
    execute!(
        stdout,
        cursor::MoveTo(0, row),
        SetForegroundColor(Color::White)
    )?;
    write!(
        stdout,
        "Tip: Type / for commands. Use /model to change model."
    )?;
    execute!(stdout, ResetColor)?;
    Ok(())
}

fn render_composer(
    stdout: &mut io::Stdout,
    row: u16,
    cols: u16,
    input: &str,
    cursor_index: usize,
) -> Result<()> {
    let width = cols.max(1) as usize;
    let fill = " ".repeat(width);
    for offset in 0..3 {
        queue_clear_line(stdout, row + offset)?;
        execute!(stdout, cursor::MoveTo(0, row + offset))?;
        write!(stdout, "{COMPOSER_BG}{fill}\x1b[0m")?;
    }
    let text = format!("{PROMPT}{input}");
    execute!(stdout, cursor::MoveTo(0, row + 1))?;
    write!(stdout, "{COMPOSER_BG}{text}\x1b[0m")?;
    let col = display_width(&format!("{PROMPT}{}", take_chars(input, cursor_index))) as u16;
    execute!(
        stdout,
        ResetColor,
        cursor::MoveTo(col.min(cols.saturating_sub(1)), row + 1)
    )?;
    Ok(())
}

fn render_footer(stdout: &mut io::Stdout, row: u16, cols: u16, session: &Session) -> Result<()> {
    queue_clear_line(stdout, row)?;
    let text = format!("{} · 100% context left · {}", session.model, session.cwd);
    execute!(
        stdout,
        cursor::MoveTo(0, row),
        SetForegroundColor(Color::DarkGrey)
    )?;
    write!(stdout, "{}", truncate_display(&text, cols as usize))?;
    execute!(stdout, ResetColor)?;
    Ok(())
}

fn finalize_submitted(start_row: u16, session: &Session, submitted: &str) -> Result<()> {
    let mut stdout = io::stdout();
    clear_runtime_area(start_row)?;
    render_header(
        &mut stdout,
        session,
        start_row,
        crossterm::terminal::size()?.0,
    )?;
    render_tip(&mut stdout, start_row + 7)?;
    execute!(stdout, cursor::MoveTo(0, start_row + 9), ResetColor)?;
    writeln!(stdout, "{PROMPT}{submitted}")?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn render_working_line() -> Result<()> {
    println!("• Working (0s • esc to interrupt)");
    io::stdout().flush()?;
    Ok(())
}

fn clear_working_line() -> Result<()> {
    let mut stdout = io::stdout();
    let (_, row) = cursor::position()?;
    queue_clear_line(&mut stdout, row.saturating_sub(1))?;
    execute!(stdout, cursor::MoveTo(0, row.saturating_sub(1)))?;
    stdout.flush()?;
    Ok(())
}

fn print_assistant_reply(reply: &str) -> Result<()> {
    println!("• {reply}");
    println!();
    Ok(())
}

fn stub_backend_reply(input: &str) -> String {
    format!("stub backend received: {input}")
}

fn clear_runtime_area(start_row: u16) -> Result<()> {
    clear_from(start_row)
}

fn clear_from(start_row: u16) -> Result<()> {
    let mut stdout = io::stdout();
    let (_, rows) = crossterm::terminal::size()?;
    for row in start_row..rows {
        queue_clear_line(&mut stdout, row)?;
    }
    execute!(stdout, cursor::MoveTo(0, start_row), ResetColor)?;
    stdout.flush()?;
    Ok(())
}

fn write_header_line(stdout: &mut io::Stdout, row: u16, text: &str) -> Result<()> {
    queue_clear_line(stdout, row)?;
    execute!(stdout, cursor::MoveTo(0, row))?;
    write!(stdout, "{text}")?;
    Ok(())
}

fn write_header_content(stdout: &mut io::Stdout, row: u16, text: &str, width: usize) -> Result<()> {
    let content = pad_display(&format!(" {text}"), width);
    write_header_line(stdout, row, &format!("│{content}│"))
}

fn queue_clear_line(stdout: &mut io::Stdout, row: u16) -> Result<()> {
    execute!(
        stdout,
        ResetColor,
        cursor::MoveTo(0, row),
        Clear(ClearType::CurrentLine)
    )?;
    Ok(())
}

fn insert_char_at(text: &mut String, index: usize, ch: char) {
    let byte_index = byte_index_for_char(text, index);
    text.insert(byte_index, ch);
}

fn remove_char_at(text: &mut String, index: usize) {
    let start = byte_index_for_char(text, index);
    let end = byte_index_for_char(text, index + 1);
    text.replace_range(start..end, "");
}

fn byte_index_for_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn take_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn pad_display(text: &str, width: usize) -> String {
    let clipped = truncate_display(text, width);
    let padding = " ".repeat(width.saturating_sub(display_width(&clipped)));
    format!("{clipped}{padding}")
}

fn truncate_display(text: &str, width: usize) -> String {
    let mut current = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let next = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current.saturating_add(next) > width {
            break;
        }
        out.push(ch);
        current = current.saturating_add(next);
    }
    out
}
