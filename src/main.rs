use std::{
    cmp::Ordering,
    error::Error,
    fs,
    fs::DirEntry,
    io,
    time::{Duration, Instant},
};
use std::path::Path;
// at the top of your source file
use std::os::macos::fs::MetadataExt;
use unix_permissions_ext::UNIXPermissionsExt;

use byte_unit::Byte;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Corner, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, List, ListItem, Row, Table, TableState},
    Frame, Terminal,
};
use users::get_user_by_uid;

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
}

impl App {
    fn new() -> App {
        App {
            dir: ".".to_string(),
            dir_list: DirectoryList {
                state: TableState::default(),
                items: vec![],
            },
            events: vec![],
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
                self.events.push(format!("{}: {}", "select_by_name", basename));
                self.dir_list.select_by_name(basename.as_str());
            }
        } else {
            self.dir_list.state.select(Some(0));
        }
    }

    fn navigate_to_parent_directory(&mut self) {
       self.navigate_to_relative_directory("..".to_string());
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
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        app.events.push("Action on selected".to_string());
                        // get the selected item
                        let sel_idx = app.dir_list.state.selected().unwrap();
                        let sel_item = &app.dir_list.items[sel_idx];
                        match sel_item {
                            DirectoryListItem::String(chg_dir) => {
                                app.navigate_to_relative_directory(chg_dir.to_owned());
                            }
                            DirectoryListItem::Entry(entry) => {
                                if entry.file_type().unwrap().is_dir() {
                                   app.navigate_to_relative_directory(entry.file_name().into_string().unwrap())
                                }
                            }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.dir_list.select_next();
                        app.events.push(format!(
                            "Next => {}",
                            app.dir_list.state.selected().unwrap()
                        ));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.dir_list.select_previous();
                        app.events.push(format!(
                            "Previous => {}",
                            app.dir_list.state.selected().unwrap()
                        ));
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        app.events.push(format!("Parent Dir"));
                        app.navigate_to_parent_directory();
                    }
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

    let rows: Vec<Row> = app
        .dir_list
        .items
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
