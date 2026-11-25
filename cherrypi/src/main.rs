mod logging;

use std::io;

use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use cherrypi_core::config::Config;
use cherrypi_core::{CherryPiPaths, ChatLogger};
use cherrypi_tui::app::{App, poll_plugin_install};
use cherrypi_tui::ui;
use cherrypi_tui::input;

/// CherryPi - An AI-powered coding agent for the terminal
#[derive(Parser, Debug)]
#[command(name = "cherrypi", version, about, long_about = None)]
struct Cli {
    /// LLM provider type (ollama, openai, anthropic)
    #[arg(short, long)]
    provider: Option<String>,

    /// API endpoint URL
    #[arg(short, long)]
    endpoint: Option<String>,

    /// API key (if required)
    #[arg(short = 'k', long)]
    api_key: Option<String>,

    /// Model name
    #[arg(short, long)]
    model: Option<String>,

    /// Path to a config file (TOML, or legacy JSON if it ends in .json)
    #[arg(short, long)]
    config: Option<String>,

    /// Plugin directory (can be specified multiple times)
    #[arg(short = 'P', long = "plugin-dir")]
    plugin_dirs: Vec<String>,

    /// Working directory
    #[arg(short = 'C', long)]
    working_dir: Option<String>,

    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,
}

/// Shuts the Servo render thread down when `main` returns, on any path. Skipping
/// this can crash during process teardown.
struct RenderShutdown;

impl Drop for RenderShutdown {
    fn drop(&mut self) {
        cherrypi_agent::render::shutdown_renderer();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _render_shutdown = RenderShutdown;
    let cli = Cli::parse();

    // Initialize platform-standard directories and config
    let paths = CherryPiPaths::initialize()?;
    let created = paths.ensure_default_config()?;
    if created {
        eprintln!("Created default config at {}", paths.config_file.display());
    }

    // Initialize logging
    init_logging(&paths, cli.debug)?;

    // Load configuration
    let mut config = if let Some(config_path) = &cli.config {
        Config::load_file(std::path::Path::new(config_path)).map_err(|e| anyhow::anyhow!(e))?
    } else {
        load_config(&paths)?
    };

    // Override with CLI args
    if let Some(provider) = cli.provider { config.provider.provider_type = provider; }
    if let Some(endpoint) = cli.endpoint { config.provider.endpoint = endpoint; }
    if let Some(api_key) = cli.api_key { config.provider.api_key = Some(api_key); }
    if let Some(model) = cli.model { config.provider.model = model; }
    if let Some(dir) = cli.working_dir {
        config.agent.working_dir = Some(dir);
    }
    for dir in cli.plugin_dirs {
        config.plugins.plugin_dirs.push(dir);
    }

    // Create the app
    let chat_logger = ChatLogger::new(
        paths.log_dir.clone(),
        &std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".to_string()),
    )?;
    let mut app = App::new(config, &paths, chat_logger).map_err(|e| anyhow::anyhow!(e))?;

    // Load available models in background
    app.load_models().await;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Main event loop
    let result = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
    )?;
    terminal.show_cursor()?;

    result?;
    Ok(())
}

async fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        app.tick_status_bar();
        terminal.draw(|f| ui::render(f, app))?;

        if app.should_quit {
            return Ok(());
        }

        // ── Processing state: plugin install vs agent vs Esc ──
        if app.is_processing {
            app.tick_spinner();

            if app.is_plugin_installing() {
                // Plugin install is a background thread — poll it non-blockingly
                let still_running = poll_plugin_install(app);
                if !still_running {
                    continue;
                }
                // Still running: wait for Esc to cancel, then loop
                if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                    if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                        if key.code == KeyCode::Esc {
                            app.cancel_processing();
                            continue;
                        }
                    }
                }
                continue;
            }

            // Get the user input that started this processing
            let user_input = app.pending_input.clone();

            let cancel = app.cancel_flag.clone();

            // The turn runs on its own handle to the agent, so the TUI can keep
            // redrawing (spinner, live tool calls) while it works. It races
            // against Esc; the redraw ticker runs until one of them finishes.
            let handle = app.agent.clone();
            let mut turn = Box::pin(async move {
                let mut agent = handle.lock().unwrap();
                agent.process_message(&user_input, Some(cancel)).await
            });
            let mut esc = Box::pin(wait_for_esc());
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));

            loop {
                tokio::select! {
                    result = &mut turn => {
                        app.finish_processing(result);
                        break;
                    }
                    _ = &mut esc => {
                        app.cancel_processing();
                        break;
                    }
                    _ = ticker.tick() => {
                        app.tick_spinner();
                        app.tick_status_bar();
                        app.drain_tool_calls();
                        terminal.draw(|f| ui::render(f, app))?;
                    }
                }
            }
            continue;
        }

        // ── Idle state: normal input ──
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.should_quit = true;
                        continue;
                    }

                    if key.code == KeyCode::Char('n')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.agent.lock().unwrap().reset_conversation();
                        app.messages.clear();
                        app.messages.push(cherrypi_tui::app::DisplayMessage {
                            role: cherrypi_core::message::MessageRole::System,
                            content: "New conversation started.".to_string(),
                            kind: cherrypi_tui::app::MessageKind::Normal,
                            timestamp: chrono::Utc::now(),
                            is_error: false,
                        });
                        continue;
                    }

                    let needs_async = input::handle_input(app, key);
                    if needs_async && app.input.trim().starts_with('/') {
                        let input = app.input.clone();
                        app.input.clear();
                        app.history_index = None;
                        app.handle_command(&input).await;
                        // If start_processing was called, next loop
                        // iteration enters the processing branch
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

/// Future that completes when the user presses Escape.
/// Does not touch the App so it can race with agent futures.
async fn wait_for_esc() {
    loop {
        if crossterm::event::poll(std::time::Duration::from_millis(100))
            .unwrap_or(false)
        {
            if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                if key.code == KeyCode::Esc {
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Find and load the first config that exists: project-local TOML, project-local
/// legacy JSON, user TOML, then user legacy JSON. Falls back to defaults.
fn load_config(paths: &CherryPiPaths) -> anyhow::Result<Config> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".cherrypi.toml"));
        candidates.push(cwd.join(".cherrypi.json"));
    }
    candidates.push(paths.config_file.clone());
    candidates.push(paths.legacy_config_file.clone());

    match candidates.into_iter().find(|path| path.exists()) {
        Some(path) => Config::load_file(&path).map_err(|e| anyhow::anyhow!(e)),
        None => Ok(Config::default()),
    }
}

/// Set up file-only logging (JSONL to disk).
/// Stderr is NOT used because the TUI owns the terminal.
fn init_logging(paths: &CherryPiPaths, debug: bool) -> anyhow::Result<()> {
    let log_level = if debug { "debug" } else { "info" };
    let env_filter = EnvFilter::new(format!("cherrypi={}", log_level));

    // Size-rotating JSONL file writer: 25 MB max, 7 compressed archives
    let file_writer = logging::RotatingJsonlWriter::new(
        paths.log_dir.clone(),
        "cherrypi",
        "jsonl",
        25 * 1024 * 1024, // 25 MB
        7,                 // keep 7 archives
    )?;

    // JSON layer for file output only (stderr would corrupt the TUI)
    let json_layer = fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_filter(env_filter);

    tracing_subscriber::registry()
        .with(json_layer)
        .init();

    tracing::info!(
        log_dir = %paths.log_dir.display(),
        "Logging initialized - JSONL logs with 25MB rotation, 7 compressed archives retained"
    );

    Ok(())
}
