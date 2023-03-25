extern crate byte_unit;

use std::{
    cmp::Ordering,
    error::Error,
    fs,
    fs::DirEntry,
    io,
    time::{Duration, Instant},
};
//use std::os::macos::fs::MetadataExt;
use std::fs::Metadata;
use unix_permissions_ext::UNIXPermissionsExt;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{
    backend::{Backend, CrosstermBackend},
    Frame,
    layout::{Constraint, Corner, Direction, Layout},
    style::{Color, Modifier, Style},
    Terminal,
    text::{Span, Spans}, widgets::{Block, Borders, List, ListItem, ListState},
};
use tui::widgets::{Row, Table, TableState};
use byte_unit::Byte;

#[derive(Debug)]
enum DirectoryListItem {
    Entry(DirEntry),
    String(String)
}

/// This struct holds the current state of the app. In particular, it has the `items` field which is a wrapper
/// around `ListState`. Keeping track of the items state let us render the associated widget with its state
/// and have access to features such as natural scrolling.
#[derive(Debug)]
struct DirectoryList {
    state: TableState,
    items: Vec<DirectoryListItem>,
}

impl DirectoryList {
    fn refresh(&mut self, dir: &str) {
        self.items.clear();
        // read all the items in the directory
        self.items = fs::read_dir(dir).unwrap()
            .into_iter()
            .map(|x| x.unwrap())
            .map(|x| DirectoryListItem::Entry(x))
            .collect();
        self.items.insert(0, DirectoryListItem::String("..".to_string()));
        self.items.sort_by(|a, b| DirectoryList::compare_dir_items(a, b));
    }

    fn compare_dir_items(a: &DirectoryListItem, b: &DirectoryListItem) -> Ordering {
        match (a, b) {
            (DirectoryListItem::String(a_str), DirectoryListItem::String(b_str)) => {
                a_str.cmp(b_str)
            },
            (DirectoryListItem::String(_), DirectoryListItem::Entry(_)) => {
                Ordering::Less
            },
            (DirectoryListItem::Entry(_), DirectoryListItem::String(_)) => {
                Ordering::Greater
            },
            (DirectoryListItem::Entry(a), DirectoryListItem::Entry(b)) => {
                let a_file_type = a.file_type().unwrap();
                let b_file_type = b.file_type().unwrap();

                return if a_file_type.is_dir() && !b_file_type.is_dir() {
                    Ordering::Less
                } else if !a_file_type.is_dir() && b_file_type.is_dir() {
                    Ordering::Greater
                } else {
                    a.file_name().cmp(&b.file_name())
                }
            }
        }
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i < self.items.len() - 1 {
                    i + 1
                } else {
                    self.items.len() - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i > 0 {
                    i - 1
                } else {
                    0
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn unselect(&mut self) {
        self.state.select(None);
    }
}

struct App {
    dir: String,
    dir_list: DirectoryList,
    events: Vec<String>,
}

impl App {
    fn new() -> App {
        App {
            dir: ".".to_string(),
            dir_list: DirectoryList {
                state: TableState::default(),
                items: vec!(),
            },
            events: vec!(),
        }
    }

    // Do something every so often
    fn on_tick(&mut self) {
    }
}


fn main() -> Result<(), Box<dyn Error>> {
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let tick_rate = Duration::from_millis(250);
    let app = App::new();
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
        println!("{:?}", err)
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();

    // read the directory contents
    app.dir = ".".to_string();
    app.dir_list.refresh(app.dir.as_str());

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('h') | KeyCode::Left => {
                        app.dir_list.unselect();
                        app.events.push("Unselect".to_string());
                    },
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.dir_list.next();
                        app.events.push(format!("Next => {}", app.dir_list.state.selected().unwrap()));
                    },
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.dir_list.previous();
                        app.events.push(format!("Previous => {}", app.dir_list.state.selected().unwrap()));
                    },
                    _ => {}
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
    // Create two chunks with equal horizontal screen space
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(f.size());

    let style = Style::default();
    let dir_style = style.add_modifier(Modifier::BOLD);
    let link_style = style.add_modifier(Modifier::ITALIC);

    let rows: Vec<Row> = app.dir_list.items
        .iter()
        .map(|item| {
            match item {
                DirectoryListItem::String(item) => {
                    let file_name = item;
                    Row::new(vec![file_name.as_str()]).style(dir_style)
                }
                DirectoryListItem::Entry(item) => {
                    // The item gets its own line
                    let file_name = item.file_name().into_string().unwrap();
                    // determine the type of file (directory, symlink, etc.)
                    let mut style = Style::default();
                    if item.file_type().unwrap().is_dir() {
                        style = dir_style;
                    }
                    if item.file_type().unwrap().is_symlink() {
                        style = link_style
                    }
                    let mut filesize_str = "".to_string();
                    if !item.file_type().unwrap().is_dir() {
                        // determine the size
                        let file_size = item.metadata().unwrap().len();
                        let byte = Byte::from_bytes(file_size.into());
                        let adjusted_byte = byte.get_appropriate_unit(false);
                        filesize_str = adjusted_byte.to_string();
                    }
                    let perms = item.metadata().unwrap().permissions();
                    let perms_str = perms.stringify();
                    let user_perms = perms_str[0..3].to_string();
                    let group_perms = perms_str[3..6].to_string();

                    Row::new(vec![file_name, filesize_str, user_perms, group_perms]).style(style)
                }
            }
        })
        .collect();

    let table = Table::new(rows)
        .header(
            Row::new(vec!["Name", "Size", "User", "Group", "rwx", "rwx", "rwx"])
                .style(Style::default().fg(Color::Yellow))
                .bottom_margin(1),
        )
        .highlight_style(style.bg(Color::Gray).fg(Color::DarkGray))
        .block(Block::default().title(app.dir.as_str()).borders(Borders::ALL))
        .widths(&[
            Constraint::Length(40),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3)
        ]);
    f.render_stateful_widget(table, chunks[0], &mut app.dir_list.state);

    let events: Vec<ListItem> = app
        .events
        .iter()
        .map(|event| {
            let log = Spans::from(vec![Span::raw(event)]);

            ListItem::new(vec![log])
        })
        .collect();

    let events_list = List::new(events)
        .block(Block::default().borders(Borders::ALL).title("List"))
        .start_corner(Corner::TopLeft);

    f.render_widget(events_list, chunks[1]);
}
