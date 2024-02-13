use std::{io, io::{BufRead, BufReader}};
use std::fs::File;
use std::path::Path;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use crossterm::event::KeyModifiers;
use thiserror::Error;
use anyhow::Result;
use tui::{
    backend::{Backend, CrosstermBackend},
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    Terminal,
    text::{Span, Spans}, widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table, Wrap},
};
use log::{debug, info, warn};
use log::LevelFilter;
use log4rs::append::file::FileAppender;
use log4rs::encode::pattern::PatternEncoder;
use log4rs::config::{Appender, Root};

use dir_list::*;

mod dir_list;

const TICK_RATE_MILLIS: u64 = 250;
const SNIPPET_LINES: usize = 50;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("unable to access directory")]
    IoError(#[from] io::Error),
    #[error("unable to watch directory for changes")]
    WatchError,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[clap(index = 1)]
    dir_name: Option<String>,
    #[arg(short, long, default_value_t = 2)]
    log: u8
}

struct App {
    dir: String,
    dir_list: DirectoryList,
    preview: Vec<String>,
    show_popup_sort: bool,
}

impl App {
    fn new(dir_name: String) -> App {
        // create the app
        let mut app = Self {
            dir: dir_name.clone(),
            dir_list: DirectoryList::new(dir_name.clone()),
            preview: vec![],
            show_popup_sort: false,
        };
        app.set_dir(dir_name);
        app
    }

    fn set_dir(&mut self, new_dir: String) {
        self.dir = Path::new(new_dir.as_str()).canonicalize()
            .expect("unable to canonicalize new directory")
            .to_str()
            .expect("unable to convert new directory to string")
            .to_string();
        self.dir_list.dir = self.dir.clone();

        self.dir_list.refresh().expect("unable to refresh");
        self.dir_list.watch().expect("unable to watch");
    }

    /// Do something every so often
    fn on_tick(&mut self) {
        // check if filesystem has changed
        let mut fs_events: Vec<notify::Event> = vec![];
        // drain the dir_watch channel
        loop {
            if let Some(rx) = self.dir_list.dir_watch_rx.as_mut() {
                match rx.try_recv() {
                    Ok(event) => {
                        fs_events.push(event.clone());
                        debug!("FS ev: {:?}:{:?}", event.kind, event.paths);
                    },
                    Err(TryRecvError::Empty) => {
                        break;
                    },
                    Err(TryRecvError::Disconnected) => {
                        break;
                    }
                }
            }
        }
        if fs_events.len() > 0 {
            let _result = self.dir_list.smart_refresh(fs_events);
        }
        // check for size notifications
        loop {
            if let Some(rx) = self.dir_list.dir_size_rx.as_mut() {
                match rx.try_recv() {
                    Ok(size_notify) => {
                        for item in &mut self.dir_list.items {
                            match item {
                                DirectoryListItem::Entry(e) => {
                                    if e.name == size_notify.name {
                                        e.size = Some(size_notify.size);
                                        break;
                                    }
                                }
                                DirectoryListItem::ParentDir(_) => {}
                            }
                        }
                    },
                    Err(TryRecvError::Empty) => {
                        break;
                    },
                    Err(TryRecvError::Disconnected) => {
                        break;
                    }
                }
            }
        }
    }

    /// Move to a new directory -- relative paths are ok, absolute paths are ok.
    fn navigate_to_relative_directory(&mut self, chg_dir: String) -> Result<()> {
        // save the current info
        let cur_path_str = &self.dir.clone();
        let cur_path = Path::new(cur_path_str);

        let chg_path = cur_path.join(chg_dir).canonicalize()?;

        // update the current info
        self.set_dir(chg_path.to_str()
            .expect("unable to convert chg_path to string")
            .to_string());

        let cur_path_str = cur_path.to_str()
            .expect("unable to convert cur_path to string")
            .to_string();
        let chg_path_str = chg_path.to_str()
            .expect("unable to convert chg_path to string")
            .to_string();

        if cur_path_str.contains(&chg_path_str) {
            // going to parent dir, try to select the proper child
            if let Some(basename) = cur_path_str.strip_prefix(chg_path_str.as_str()) {
                let mut basename = basename.to_string();
                if let Some(new_basename) = basename.strip_prefix("/") {
                    basename = new_basename.to_string();
                }
                self.dir_list.select_by_name(basename.as_str());
            }
        } else {
            self.dir_list.state.select(Some(0));
        }

        Ok(())
    }

    /// Move to the parent of the current directory.
    fn navigate_to_parent_directory(&mut self) -> Result<()> {
        self.navigate_to_relative_directory("..".to_string())?;

        Ok(())
    }

    /// Load a preview of the selected file
    fn load_preview(&mut self) -> Result<()> {
        if !self.dir_list.selection_changed {
            // the existing preview is still valid
            return Ok(());
        }
        self.preview.clear();
        if let Some(sel_idx) = self.dir_list.state.selected() {
            match &self.dir_list.items[sel_idx] {
                DirectoryListItem::Entry(entry) => {
                    if entry.file_type.is_file() {
                        let cur_dir = self.dir.clone();
                        let cur_path = Path::new(&cur_dir);
                        let entry_path = cur_path.join(&entry.name);
                        if let Some(mime_type) = tree_magic_mini::from_filepath(entry_path.as_path()) {
                            if mime_type.starts_with("text") {
                                let file = File::open(entry_path)?;
                                let reader = BufReader::new(file);
                                for (index, line) in reader.lines().enumerate() {
                                    if index > SNIPPET_LINES { break; }
                                    self.preview.push(line
                                        .expect("unable to add line to preview"));
                                }
                            }
                        }
                    } else if entry.file_type.is_dir() {
                       // TODO: show directory contents
                    }
                }
                DirectoryListItem::ParentDir(_) => {}
            }
        }

        self.dir_list.selection_changed = false;
        Ok(())
    }
}

enum KeyInputResult {
    Continue,
    Stop,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut log_level = LevelFilter::Warn;
    if args.log == 0 {
        log_level = LevelFilter::Off;
    } else if args.log == 1 {
        log_level = LevelFilter::Error;
    } else if args.log == 2 {
        // default (WARN)
    } else if args.log == 3 {
        log_level = LevelFilter::Info;
    } else if args.log == 4 {
        log_level = LevelFilter::Debug;
    } else if args.log == 5 {
        log_level = LevelFilter::Trace;
    }

    // setup logging
    let logfile = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new("{l} {d} {t} - {m}{n}")))
        .build("/tmp/lsls.log")?;

    let config = log4rs::config::Config::builder()
        .appender(Appender::builder().build("logfile", Box::new(logfile)))
        .build(Root::builder()
            .appender("logfile")
            .build(log_level))?;

    log4rs::init_config(config)?;

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let tick_rate = Duration::from_millis(TICK_RATE_MILLIS);
    let app = App::new(args.dir_name.unwrap_or(".".to_string()));
    let res = run_app(&mut terminal, app, tick_rate);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

fn handle_input_sort_popup(app: &mut App, key: KeyEvent) -> KeyInputResult {
    match key.code {
        KeyCode::Char('q') => {
            app.show_popup_sort = false;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            app.dir_list.sort_by = SortBy::all()[
                app.dir_list.sort_by_list_state
                    .selected()
                    .expect("unable to identify selected sort_by item")
                ].clone();
                debug!("sort_by changed to {}", app.dir_list.sort_by.to_string());
                app.dir_list.sort();
            app.show_popup_sort = false;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(mut selected_idx) = app.dir_list.sort_by_list_state.selected() {
                selected_idx = selected_idx + 1;
                if selected_idx < SortBy::all().len() {
                    app.dir_list.sort_by_list_state.select(Some(selected_idx));
                }
            } else {
                app.dir_list.sort_by_list_state.select(Some(0));
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(mut selected_idx) = app.dir_list.sort_by_list_state.selected() {
                if selected_idx > 0 {
                    selected_idx = selected_idx - 1;
                    app.dir_list.sort_by_list_state.select(Some(selected_idx));
                }
            } else {
                app.dir_list.sort_by_list_state.select(Some(0));
            }
        }
        _ => {}
    }
    return KeyInputResult::Continue;
}

fn handle_input(app: &mut App, key_event: KeyEvent) -> KeyInputResult {
    if app.show_popup_sort {
        return handle_input_sort_popup(app, key_event);
    }

    match key_event.code {
        KeyCode::Char('q') => {
            // QUIT -> bail
            return KeyInputResult::Stop;
        },
        KeyCode::Char('s') => {
            app.show_popup_sort = !app.show_popup_sort;
            return KeyInputResult::Continue;
        },
        KeyCode::Char('i') => {
            // TODO: show info dialog
            return KeyInputResult::Continue;
        },
        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') => {
            // get the selected item
            if let Some(sel_idx) = app.dir_list.state.selected() {
                match &app.dir_list.items[sel_idx] {
                    DirectoryListItem::ParentDir(chg_dir) => {
                        app.navigate_to_relative_directory(chg_dir.to_owned()).ok();
                    }
                    DirectoryListItem::Entry(entry) => {
                        if entry.file_type.is_dir() {
                            app.navigate_to_relative_directory(entry.name.clone()).ok();
                        } else {
                            // open the file (unless `l` key was pressed -- that would just be weird)
                            if key_event.code != KeyCode::Char('l') {
                                let cur_path = Path::new(&app.dir);
                                let entry_path = cur_path.join(&entry.name);
                                let _result = opener::open(entry_path.as_path());
                            }
                        }
                    }
                }
            }
            return KeyInputResult::Continue;
        },

        // the remaining keys should refresh the preview pane
        KeyCode::Down | KeyCode::Char('j') => {
            app.dir_list.select_next();
        },
        KeyCode::Up | KeyCode::Char('k') => {
            app.dir_list.select_previous();
        },
        KeyCode::Left | KeyCode::Char('h') => {
            app.navigate_to_parent_directory().ok();
        },
        KeyCode::Char('g') => {
            app.dir_list.select_first();
        },
        KeyCode::Char('G') => {
            app.dir_list.select_last();
        },
        // TODO: next-page (CTRL+f)
        KeyCode::Char('f') => {
            match key_event.modifiers {
                KeyModifiers::CONTROL => {
                    // TODO: next page
                },
                _ => {}
            }
        },
        // TODO: prev-page (CTRL+b)
        KeyCode::Char('b') => {
            match key_event.modifiers {
                KeyModifiers::CONTROL => {
                    // TODO: previous page
                },
                _ => {}
            }
        }
        _ => {}
    }

    app.load_preview().ok();

    return KeyInputResult::Continue;
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if crossterm::event::poll(timeout)? {
            if let crossterm::event::Event::Key(key) = event::read()? {
                match handle_input(&mut app, key) {
                    KeyInputResult::Stop => {
                        return Ok(());
                    }
                    KeyInputResult::Continue => {}
                }
            }
        }
        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app: &mut App) {
    let style = Style::default();

    // Create two chunks with equal horizontal screen space
    let h_panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
        .split(f.size());

    let v_panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
        .split(h_panes[1]);

    let rows: Vec<Row> = app
        .dir_list
        .items
        .iter()
        .map(|item| {
            let row: Row = (*item).clone().into();
            row
        })
        .collect();

    let table = Table::new(rows)
        .header(
            Row::new(vec!["Name", "Size", "Modified", "User", "Group", "Usr", "Grp", "Oth"])
                .style(Style::default().fg(Color::Yellow))
                .bottom_margin(1),
        )
        .highlight_style(style.bg(Color::Gray).fg(Color::DarkGray))
        .block(
            Block::default()
                .title(app.dir.as_str())
                .borders(Borders::ALL),
        )
        .widths(&[
            Constraint::Length(20),     // name
            Constraint::Length(10),     // size
            Constraint::Length(19),     // date
            Constraint::Length(12),     // user
            Constraint::Length(5),      // group
            Constraint::Length(3),      // usr (mask)
            Constraint::Length(3),      // grp (mask)
            Constraint::Length(3),      // oth (mask)
        ]);
    f.render_stateful_widget(table, h_panes[0], &mut app.dir_list.state);

    let preview_block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default())
        .title("Preview");

    let preview_text: Vec<Spans> = app.preview
        .iter()
        .map(|s| Spans::from(s.as_str()))
        .collect();

    // wrap single-lined files
    let mut preview_wrap = Wrap { trim: false };
    if preview_text.len() <= 1 {
        preview_wrap = Wrap { trim: true };
    }

    let preview_paragraph = Paragraph::new(preview_text)
        .style(Style::default())
        .block(preview_block)
        .wrap(preview_wrap)
        .alignment(Alignment::Left);

    f.render_widget(preview_paragraph, v_panes[0]);

    if app.show_popup_sort {
        let sort_by_items: Vec<ListItem> = SortBy::all()
            .iter()
            .map(|sort_by| {
                let span = Spans::from(vec![Span::raw(sort_by.to_string())]);

                ListItem::new(vec![span])
            })
            .collect();
        let sort_by_list = List::new(sort_by_items)
            .highlight_style(style.bg(Color::Gray).fg(Color::DarkGray))
            .block(Block::default().title("Sort By").borders(Borders::ALL));
        let area = centered_rect(30, 50, f.size());
        if app.dir_list.sort_by_list_state.selected() == None {
            app.dir_list.sort_by_list_state.select(Some(0));
        }
        f.render_widget(tui::widgets::Clear, area);
        f.render_stateful_widget(sort_by_list, area, &mut app.dir_list.sort_by_list_state);
    }
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
                .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
                .as_ref(),
        )
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_navigate_to_relative_directory() {
        let mut app = App::new("/".to_string());
        app.navigate_to_relative_directory("tmp".to_string()).unwrap();
        println!("relative dir: {}", app.dir);
        assert_eq!("/tmp".to_string(), app.dir);
    }

    #[test]
    fn test_navigate_to_parent_directory() {
        let mut app = App::new("/tmp".to_string());
        app.navigate_to_parent_directory().unwrap();
        println!("parent dir: {}", app.dir);
        assert_eq!("/".to_string(), app.dir);
    }

    #[test]
    fn test_navigate_to_absolute_directory() {
        let mut app = App::new(".".to_string());
        app.navigate_to_relative_directory("/tmp".to_string()).unwrap();
        println!("absolute dir: {}", app.dir);
        assert_eq!("/tmp".to_string(), app.dir);
    }
}