use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io;

use crate::config::{load_config, AgentType, Config, Construct, LLMConfig};
use crate::jack::jack_in;
use crate::ollama;

pub fn show_config() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let config = load_config()?;
    let selected = run_app(&mut terminal, config);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Some(construct_name) = selected? {
        jack_in(Some(&construct_name), &[])?;
    }

    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, config: Config) -> Result<Option<String>> {
    let mut selected_index = 0;
    loop {
        terminal.draw(|f| ui(f, &config, selected_index))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                KeyCode::Enter => {
                    if !config.constructs.is_empty() {
                        return Ok(Some(config.constructs[selected_index].name.clone()));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if !config.constructs.is_empty() {
                        if selected_index > 0 {
                            selected_index -= 1;
                        } else {
                            selected_index = config.constructs.len() - 1;
                        }
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !config.constructs.is_empty() {
                        if selected_index + 1 < config.constructs.len() {
                            selected_index += 1;
                        } else {
                            selected_index = 0;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn ui(f: &mut Frame, config: &Config, selected_index: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3),      // Title
            Constraint::Percentage(35), // Constructs list
            Constraint::Percentage(55), // Flow diagram
            Constraint::Length(3),      // Help
        ])
        .split(f.size());

    let title = Paragraph::new("crig - construct-rig")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    render_constructs_list(f, config, selected_index, chunks[1]);
    render_selected_construct(f, config, selected_index, chunks[2]);

    let help = Paragraph::new("q / Esc: Quit  |  k / j: Navigate  |  Enter: Jack in")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(help, chunks[3]);
}

fn render_constructs_list(f: &mut Frame, config: &Config, selected_index: usize, area: Rect) {
    let items: Vec<ListItem> = config
        .constructs
        .iter()
        .enumerate()
        .map(|(i, construct)| {
            let marker = if i == selected_index { "▶ " } else { "  " };
            let content = format!("{}🧰 {}", marker, construct.name);
            let style = if i == selected_index {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(content).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().title("Constructs").borders(Borders::ALL))
        .style(Style::default().fg(Color::White));

    f.render_widget(list, area);
}

fn render_selected_construct(f: &mut Frame, config: &Config, selected_index: usize, area: Rect) {
    let block = Block::default()
        .title("Selected Construct Flow")
        .borders(Borders::ALL);
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if let Some(construct) = config.constructs.get(selected_index) {
        let flow_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(46),
                Constraint::Length(4),
                Constraint::Length(6),
                Constraint::Length(4),
                Constraint::Percentage(46),
            ])
            .split(inner_area);

        render_agent_box(f, construct, flow_chunks[0]);
        render_arrow(f, flow_chunks[1]);
        render_connection(f, construct, flow_chunks[2]);
        render_arrow(f, flow_chunks[3]);
        render_llm_box(f, construct, flow_chunks[4]);
    } else {
        let paragraph = Paragraph::new(Text::from("No construct selected"))
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Center);
        f.render_widget(paragraph, inner_area);
    }
}

fn render_agent_box(f: &mut Frame, construct: &Construct, area: Rect) {
    let agent_icon = match construct.agent_type {
        AgentType::ClaudeCode => "🤖",
        AgentType::Custom => "⚙️",
    };

    let agent_name = match construct.agent_type {
        AgentType::ClaudeCode => "Claude Code",
        AgentType::Custom => "Custom",
    };

    let lines = vec![
        Line::from(vec![Span::styled(
            "💻 Local PC",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Green),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("{} Agent", agent_icon),
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::raw(agent_name)]),
    ];

    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center);

    f.render_widget(paragraph, area);
}

fn render_arrow(f: &mut Frame, area: Rect) {
    let arrow = vec![
        Line::from(""),
        Line::from(""),
        Line::from(vec![Span::styled(
            ">>",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
    ];

    let paragraph = Paragraph::new(Text::from(arrow)).alignment(Alignment::Center);

    f.render_widget(paragraph, area);
}

fn render_connection(f: &mut Frame, construct: &Construct, area: Rect) {
    let endpoint = match &construct.llm_config {
        LLMConfig::ClaudeApi { .. } => None,
        LLMConfig::AnthropicCompatible { endpoint, .. }
        | LLMConfig::Ollama { endpoint, .. } => Some(endpoint.as_str()),
    };

    let marker = match endpoint {
        Some(ep) if ollama::endpoint_is_local(ep) => {
            Span::styled("🔌", Style::default().fg(Color::Green))
        }
        _ => Span::styled(
            "◈",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let lines = vec![Line::from(""), Line::from(""), Line::from(vec![marker])];

    let paragraph = Paragraph::new(Text::from(lines)).alignment(Alignment::Center);

    f.render_widget(paragraph, area);
}

fn render_llm_box(f: &mut Frame, construct: &Construct, area: Rect) {
    let lines = match &construct.llm_config {
        LLMConfig::ClaudeApi { api_key, model } => {
            let key_icon = if api_key.is_some() { "🔐" } else { "🔑" };
            let key_status = if api_key.is_some() { "Config" } else { "Env" };

            vec![
                Line::from(vec![Span::styled(
                    "☁️  Claude API",
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Cyan),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "💎 Model",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::styled(
                    model.as_str(),
                    Style::default().fg(Color::Cyan),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    format!("{} API Key", key_icon),
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::raw(key_status)]),
            ]
        }
        LLMConfig::AnthropicCompatible { endpoint, model }
        | LLMConfig::Ollama { endpoint, model, .. } => {
            let (header, api_key) = match &construct.llm_config {
                LLMConfig::Ollama { api_key, .. } => ("🦙 Ollama", api_key.as_ref()),
                _ => ("🔌 Anthropic-compatible", None),
            };
            let mut lines = vec![
                Line::from(vec![Span::styled(
                    header,
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Green),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "💎 Model",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::styled(
                    model.as_str(),
                    Style::default().fg(Color::Cyan),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "🔌 Endpoint",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::styled(
                    endpoint.as_str(),
                    Style::default().fg(Color::Yellow),
                )]),
            ];
            if matches!(&construct.llm_config, LLMConfig::Ollama { .. }) {
                let (icon, status) = if api_key.is_some() {
                    ("🔐", "Config")
                } else {
                    ("🔑", "Default")
                };
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    format!("{} API Token", icon),
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(vec![Span::raw(status)]));
            }
            lines
        }
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center);

    f.render_widget(paragraph, area);
}
