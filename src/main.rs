use std::{
    cmp::Ordering,
    error::Error,
    fs,
    fs::{DirEntry, File},
    io,
    io::{BufRead, BufReader},
    os::macos::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};
// at the top of your source file
use unix_permissions_ext::UNIXPermissionsExt;

use byte_unit::Byte;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Corner, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};
use tui::widgets::Wrap;
use users::get_user_by_uid;

const MAX_EVENTS: usize = 5;
const TICK_RATE_MILLIS: u64 = 250;
const SNIPPET_LINES: usize = 50;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[clap(index = 1)]
    dir_name: Option<String>,
}

#[derive(Debug)]
enum DirectoryListItem {
    Entry(DirEntry),
    String(String),
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
        self.items = fs::read_dir(dir)
            .unwrap()
            .into_iter()
            .map(|x| x.unwrap())
            .map(|x| DirectoryListItem::Entry(x))
            .collect();
        self.items
            .insert(0, DirectoryListItem::String("..".to_string()));
        self.items
            .sort_by(|a, b| DirectoryList::compare_dir_items(a, b));

        if self.state.selected() == None {
            self.state.select(Some(0));
        }
    }

    fn compare_dir_items(a: &DirectoryListItem, b: &DirectoryListItem) -> Ordering {
        match (a, b) {
            (DirectoryListItem::String(a_str), DirectoryListItem::String(b_str)) => {
                a_str.cmp(b_str)
            }
            (DirectoryListItem::String(_), DirectoryListItem::Entry(_)) => Ordering::Less,
            (DirectoryListItem::Entry(_), DirectoryListItem::String(_)) => Ordering::Greater,
            (DirectoryListItem::Entry(a), DirectoryListItem::Entry(b)) => {
                let a_file_type = a.file_type().unwrap();
                let b_file_type = b.file_type().unwrap();

                return if a_file_type.is_dir() && !b_file_type.is_dir() {
                    Ordering::Less
                } else if !a_file_type.is_dir() && b_file_type.is_dir() {
                    Ordering::Greater
                } else {
                    a.file_name().cmp(&b.file_name())
                };
            }
        }
    }

    fn select_by_name(&mut self, name: &str) {
        self.unselect();
        for (i, x) in self.items.iter().enumerate() {
            match x {
                DirectoryListItem::Entry(entry) => {
                   if name.eq(entry.file_name().into_string().unwrap().as_str()) {
                       self.state.select(Some(i));
                       break;
                   }
                }
                _ => {}
            }
        }
    }

    fn select_next(&mut self) {
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

    fn select_previous(&mut self) {
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
    event_list_state: ListState,
    file_snippet: Vec<String>
}

impl App {
    fn new(dir_name: String) -> App {
        App {
            dir: dir_name,
            dir_list: DirectoryList {
                state: TableState::default(),
                items: vec![],
            },
            events: vec![],
            event_list_state: ListState::default(),
            file_snippet: vec![]
        }
    }

    // Do something every so often
    fn on_tick(&mut self) {
        self.dir = Path::new(self.dir.as_str()).canonicalize().unwrap().to_str().unwrap().to_string();
        self.dir_list.refresh(self.dir.as_str());
    }

    fn navigate_to_relative_directory(&mut self, chg_dir: String) {
        // save the current info
        let cur_path_str = &self.dir.clone();
        let cur_path = Path::new(cur_path_str);
        let chg_path = cur_path.join(chg_dir).canonicalize().unwrap();

        // update the current info
        self.dir = chg_path.to_str().unwrap().to_string();
        self.dir_list.refresh(&self.dir);

        let cur_path_str = cur_path.to_str().unwrap().to_string();
        let chg_path_str = chg_path.to_str().unwrap().to_string();
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
    }

    fn navigate_to_parent_directory(&mut self) {
       self.navigate_to_relative_directory("..".to_string());
    }

    fn add_event(&mut self, event: String) {
       while self.events.len() > MAX_EVENTS {
           self.events.remove(0);
       }
        self.events.push(event);
    }

    fn load_file_snippet(&mut self) {
        self.file_snippet.clear();
        let sel_idx = self.dir_list.state.selected().unwrap();
        match &self.dir_list.items[sel_idx] {
            DirectoryListItem::Entry(entry) => {
                if !entry.file_type().unwrap().is_dir() {
                    if let Some(mime_type) = tree_magic_mini::from_filepath(&entry.path()) {
                        if mime_type.starts_with("text") {
                            let file = File::open(&entry.path()).unwrap();
                            let reader = BufReader::new(file);
                            for (index, line) in reader.lines().enumerate() {
                                if index > SNIPPET_LINES { break; }
                                self.file_snippet.push(line.unwrap());
                            }
                        }
                        self.add_event(format!("File type: {}", mime_type.to_string()));
                    }
                }
            },
            DirectoryListItem::String(_) => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = Args::parse();
    if let None = args.dir_name {
        args.dir_name = Some(".".to_string());
    }

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let tick_rate = Duration::from_millis(TICK_RATE_MILLIS);
    let app = App::new(args.dir_name.unwrap());
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

fn handle_input(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Char(' ') => {
            // get the selected item
            let sel_idx = app.dir_list.state.selected().unwrap();
            match &app.dir_list.items[sel_idx] {
                DirectoryListItem::String(chg_dir) => {
                    app.navigate_to_relative_directory(chg_dir.to_owned());
                },
                DirectoryListItem::Entry(entry) => {
                    if entry.file_type().unwrap().is_dir() {
                        app.navigate_to_relative_directory(entry.file_name().into_string().unwrap())
                    } else {
                        opener::open(entry.path()).ok();
                    }
                }
            }
        },
        KeyCode::Down | KeyCode::Char('j') => {
            app.dir_list.select_next();
            app.load_file_snippet();
        },
        KeyCode::Up | KeyCode::Char('k') => {
            app.dir_list.select_previous();
            app.load_file_snippet();
        },
        KeyCode::Left | KeyCode::Char('h') => {
            app.navigate_to_parent_directory();
        },
        _ => {}
    }
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();

    // read the directory contents
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
                    _ => handle_input(&mut app, key)
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
    let h_panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(f.size());

    let v_panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
        .split(h_panes[1]);

    let style = Style::default();
    let dir_style = style.add_modifier(Modifier::BOLD);
    let link_style = style.add_modifier(Modifier::ITALIC);

    let rows: Vec<Row> = app
        .dir_list
        .items
        .iter()
        .map(|item| {
            match item {
                DirectoryListItem::String(item) => {
                    let file_name = item;
                    Row::new(vec![file_name.as_str()]).style(dir_style)
                },
                DirectoryListItem::Entry(item) => {
                    // The item gets its own line
                    let file_name = item.file_name().into_string().unwrap();
                    // determine the type of file (directory, symlink, etc.)
                    let mut style = Style::default();
                    if item.file_type().unwrap().is_dir() {
                        style = dir_style;
                    }
                    if item.file_type().unwrap().is_symlink() {
                        style = link_style;
                    }
                    let mut filesize_str = "".to_string();
                    if !item.file_type().unwrap().is_dir() {
                        // determine the size
                        let file_size = item.metadata().unwrap().len();
                        let byte = Byte::from_bytes(file_size.into());
                        let adjusted_byte = byte.get_appropriate_unit(false);
                        filesize_str = adjusted_byte.to_string();
                    }
                    let meta = item.metadata().unwrap();
                    let uid = meta.st_uid();
                    let user: String = get_user_by_uid(uid)
                        .unwrap()
                        .name()
                        .to_os_string()
                        .into_string()
                        .unwrap();
                    let gid = meta.st_gid().to_string();
                    let perms = meta.permissions();
                    let perms_str = perms.stringify();
                    let user_perms = perms_str[0..3].to_string();
                    let group_perms = perms_str[3..6].to_string();
                    let other_perms = perms_str[6..9].to_string();

                    Row::new(vec![
                        file_name,
                        filesize_str,
                        user,
                        gid,
                        user_perms,
                        group_perms,
                        other_perms,
                    ])
                    .style(style)
                }
            }
        })
        .collect();

    let table = Table::new(rows)
        .header(
            Row::new(vec!["Name", "Size", "User", "Group", "Usr", "Grp", "Oth"])
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
            Constraint::Length(20),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ]);
    f.render_stateful_widget(table, h_panes[0], &mut app.dir_list.state);

    let snippet_block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default())
        .title("Snippet");

    let snippet_text: Vec<Spans> = app.file_snippet
        .iter()
        .map(|s| Spans::from(s.as_str()))
        .collect();

    // wrap single-lined files
    let mut snippet_wrap = Wrap { trim: false };
    if snippet_text.len() <= 1 {
        snippet_wrap = Wrap { trim: true };
    }

    let snippet_paragraph = Paragraph::new(snippet_text)
        .style(Style::default())
        .block(snippet_block)
        .wrap(snippet_wrap)
        .alignment(Alignment::Left);

    f.render_widget(snippet_paragraph, v_panes[0]);

    let events: Vec<ListItem> = app
        .events
        .iter()
        .map(|event| {
            let log = Spans::from(vec![Span::raw(event)]);

            ListItem::new(vec![log])
        })
        .collect();

    let events_list = List::new(events)
        .block(Block::default().borders(Borders::ALL).title("Events"))
        .start_corner(Corner::TopLeft);

    f.render_stateful_widget(events_list, v_panes[1], &mut app.event_list_state);
}
