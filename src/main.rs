use std::{cmp::Ordering, error::Error, fmt, fs, fs::{DirEntry, File}, io, io::{BufRead, BufReader}, path::Path, time::{Duration, Instant}};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
#[cfg(target_os = "linux")]
use std::os::linux::fs::MetadataExt;
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt;

use unix_permissions_ext::UNIXPermissionsExt;

use byte_unit::Byte;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use fs_extra::dir::get_size;
use thiserror::Error;
use strum_macros::EnumIter;
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Corner, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap},
    Frame, Terminal,
};
use users::get_user_by_uid;
use notify::{Watcher, RecursiveMode, watcher};

const MAX_EVENTS: usize = 5;
const TICK_RATE_MILLIS: u64 = 250;
const SNIPPET_LINES: usize = 50;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("unable to access directory")]
    DirListError(#[from] io::Error),
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[clap(index = 1)]
    dir_name: Option<String>,
}

#[derive(Debug, Clone)]
enum SortByDirection {
    Asc,
    Dec,
}

impl Default for SortByDirection {
    fn default() -> Self {
        SortByDirection::Asc
    }
}

#[derive(Debug, Clone, EnumIter)]
enum SortBy {
    TypeName(SortByDirection),
    Name(SortByDirection),
    DateTime(SortByDirection),
    Size(SortByDirection),
}

impl SortBy {
    fn all() -> Vec<SortBy> {
        vec![
            SortBy::TypeName(SortByDirection::Asc),
            SortBy::TypeName(SortByDirection::Dec),
            SortBy::DateTime(SortByDirection::Asc),
            SortBy::DateTime(SortByDirection::Dec),
            SortBy::Name(SortByDirection::Asc),
            SortBy::Name(SortByDirection::Dec),
            SortBy::Size(SortByDirection::Asc),
            SortBy::Size(SortByDirection::Dec),
        ]
    }
}

impl fmt::Display for SortBy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let output: String = match self {
            SortBy::TypeName(SortByDirection::Asc) => "TypeName (ASC)".to_string(),
            SortBy::TypeName(SortByDirection::Dec) => "TypeName (DEC)".to_string(),
            SortBy::DateTime(SortByDirection::Asc) => "DateTime (ASC)".to_string(),
            SortBy::DateTime(SortByDirection::Dec) => "DateTime (DEC)".to_string(),
            SortBy::Name(SortByDirection::Asc) => "Name (ASC)".to_string(),
            SortBy::Name(SortByDirection::Dec) => "Name (DEC)".to_string(),
            SortBy::Size(SortByDirection::Asc) => "Size (ASC)".to_string(),
            SortBy::Size(SortByDirection::Dec) => "Size (DEC)".to_string(),
        };
        write!(f, "{:?}", output)
    }
}

#[derive(Debug)]
enum DirectoryListItem {
    Entry(DirEntry),
    ParentDir(String),
}

/// This struct holds the current state of the app. In particular, it has the `items` field which is a wrapper
/// around `ListState`. Keeping track of the items state let us render the associated widget with its state
/// and have access to features such as natural scrolling.
#[derive(Debug)]
struct DirectoryList {
    dir: String,
    sort_by: SortBy,
    sort_by_list_state: ListState,
    state: TableState,
    items: Vec<DirectoryListItem>,
    watcher_tx: Option<Sender<String>>,
    watcher_rx: Option<Receiver<()>>,
}

impl DirectoryList {
    fn watch(&mut self) {
        match &self.watcher_tx {
            Some(watcher_tx) => {
                // send the new directory to the watcher thread
                watcher_tx.send(self.dir.clone());
            },
            None => {
                // we need to create a new watcher thread
                let dir = self.dir.clone();
                let (dir_tx, dir_rx): (Sender<String>, Receiver<String>) = channel();
                // this is used to send updates to the watcher
                self.watcher_tx = Some(dir_tx);
                // this is used to send updates from the watcher
                let (watching_tx, watching_rx): (Sender<()>, Receiver<()>) = channel();
                self.watcher_rx = Some(watching_rx);
                thread::spawn(move || {
                    let mut dir = dir.clone();
                    let (watch_tx, watch_rx) = channel();
                    let mut watcher = watcher(watch_tx, Duration::from_secs(10)).unwrap();
                    watcher.watch(dir.clone(), RecursiveMode::Recursive).unwrap();
                    loop {
                        if let Ok(dir_event) = dir_rx.try_recv() {
                            // changed directory to watch
                            watcher.unwatch(dir.clone());
                            dir = dir_event.clone();
                            watcher.watch(dir_event, RecursiveMode::Recursive).unwrap();
                        }
                        if let Ok(_) = watch_rx.try_recv() {
                            // watched directory changed
                            // TODO: trigger self.refresh()
                            watching_tx.send(());
                        }
                    }
                });
            }
        }
    }

    fn refresh(&mut self) -> Result<(), io::Error> {
        self.items.clear();
        // read all the items in the directory
        self.items = fs::read_dir(self.dir.clone())?
            .into_iter()
            .map(|x| x.unwrap())
            .map(|x| DirectoryListItem::Entry(x))
            .collect();
        self.items
            .insert(0, DirectoryListItem::ParentDir("..".to_string()));
        self.items
            .sort_by(|a, b| DirectoryList::compare_dir_items(a, b, self.sort_by.clone()));

        if self.state.selected() == None {
            self.state.select(Some(0));
        }

        Ok(())
    }

    /// Sort the DirectoryListItems based on the `sort_by` parameter.
    fn compare_dir_items(a: &DirectoryListItem, b: &DirectoryListItem, sort_by: SortBy) -> Ordering {
        match (a, b) {
            (DirectoryListItem::ParentDir(a_str), DirectoryListItem::ParentDir(b_str)) => {
                a_str.cmp(b_str)
            }
            (DirectoryListItem::ParentDir(_), DirectoryListItem::Entry(_)) => Ordering::Less,
            (DirectoryListItem::Entry(_), DirectoryListItem::ParentDir(_)) => Ordering::Greater,
            (DirectoryListItem::Entry(a), DirectoryListItem::Entry(b)) => {
                #[allow(unused_assignments)]
                    let mut sort_by_direction = SortByDirection::default();
                let mut retval = match sort_by {
                    SortBy::TypeName(direction) => {
                        sort_by_direction = direction;
                        if a.file_type().unwrap().is_dir() && !b.file_type().unwrap().is_dir() {
                            Ordering::Less
                        } else if !a.file_type().unwrap().is_dir() && b.file_type().unwrap().is_dir() {
                            Ordering::Greater
                        } else {
                            a.file_name().cmp(&b.file_name())
                        }
                    }
                    SortBy::Size(direction) => {
                        sort_by_direction = direction;
                        if a.metadata().unwrap().len() < b.metadata().unwrap().len() {
                            Ordering::Less
                        } else if a.metadata().unwrap().len() > b.metadata().unwrap().len() {
                            Ordering::Greater
                        } else {
                            Ordering::Equal
                        }
                    }
                    SortBy::Name(direction) => {
                        sort_by_direction = direction;
                        a.file_name().cmp(&b.file_name())
                    }
                    SortBy::DateTime(direction) => {
                        sort_by_direction = direction;
                        a.metadata().unwrap().modified().unwrap().cmp(&b.metadata().unwrap().modified().unwrap())
                    }
                };
                // reverse the order?
                match sort_by_direction {
                    SortByDirection::Asc => {}
                    SortByDirection::Dec => retval = retval.reverse(),
                }
                retval
            }
        }
    }

    /// Select the first DirectoryListItem with the given name.
    /// If none exists, nothing will be selected.
    fn select_by_name(&mut self, name: &str) {
        self.unselect();
        for (i, x) in self.items.iter().enumerate() {
            match x {
                DirectoryListItem::Entry(entry) => {
                    let fname = entry.file_name()
                        .into_string()
                        .unwrap_or("".to_string());
                    if name.eq(fname.as_str()) {
                        self.state.select(Some(i));
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    /// Select the next item in the list, without wrapping.
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

    /// Select the previous item in the list, without wrapping.
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

    /// Unselect any previously selected item in the list.
    fn unselect(&mut self) {
        self.state.select(None);
    }
}

struct App {
    dir: String,
    dir_list: DirectoryList,
    events: Vec<String>,
    event_list_state: ListState,
    file_snippet: Vec<String>,
    sort_popup: bool,
}

impl App {
    fn new(dir_name: String) -> App {
        App {
            dir: dir_name.clone(),
            dir_list: DirectoryList {
                dir: dir_name.clone(),
                state: TableState::default(),
                items: vec![],
                watcher_tx: None,
                watcher_rx: None,
                sort_by: SortBy::TypeName(SortByDirection::Asc),
                sort_by_list_state: {
                    let mut state = ListState::default();
                    state.select(Some(0));
                    state
                },
            },
            events: vec![],
            event_list_state: ListState::default(),
            file_snippet: vec![],
            sort_popup: false,
        }
    }

    fn set_dir(&mut self, new_dir: String) {
        self.dir = Path::new(new_dir.as_str()).canonicalize().unwrap().to_str().unwrap().to_string();
        self.dir_list.dir = self.dir.clone();
        self.dir_list.refresh();
        self.dir_list.watch();
    }

    /// Do something every so often
    fn on_tick(&mut self) {
        // self.dir = Path::new(self.dir.as_str()).canonicalize().unwrap().to_str().unwrap().to_string();
        // self.dir_list.refresh(self.dir.as_str(), self.sort_by.clone()).ok();
    }

    /// Move to a new directory -- relative paths are ok, absolute paths are ok.
    fn navigate_to_relative_directory(&mut self, chg_dir: String) -> Result<(), AppError> {
        // save the current info
        let cur_path_str = &self.dir.clone();
        let cur_path = Path::new(cur_path_str);

        let chg_path = cur_path.join(chg_dir).canonicalize()?;

        // update the current info
        self.set_dir(chg_path.to_str().unwrap().to_string());

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

        Ok(())
    }

    /// Move to the parent of the current directory.
    fn navigate_to_parent_directory(&mut self) -> Result<(), AppError> {
        self.navigate_to_relative_directory("..".to_string())?;

        Ok(())
    }

    /// Add an event to the event list. Only MAX_EVENTS are stored/displayed.
    fn add_event(&mut self, event: String) {
        while self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    /// Load a snippet of the selected file into the snippet view.
    fn load_file_snippet(&mut self) -> Result<(), io::Error> {
        self.file_snippet.clear();
        if let Some(sel_idx) = self.dir_list.state.selected() {
            match &self.dir_list.items[sel_idx] {
                DirectoryListItem::Entry(entry) => {
                    if !entry.file_type().unwrap().is_dir() {
                        if let Some(mime_type) = tree_magic_mini::from_filepath(&entry.path()) {
                            if mime_type.starts_with("text") {
                                let file = File::open(&entry.path())?;
                                let reader = BufReader::new(file);
                                for (index, line) in reader.lines().enumerate() {
                                    if index > SNIPPET_LINES { break; }
                                    self.file_snippet.push(line.unwrap());
                                }
                            }
                            self.add_event(format!("File: {}, Type: {}",
                                                   entry.file_name().into_string().unwrap_or("?".to_string()),
                                                   mime_type.to_string()));
                        }
                    }
                }
                DirectoryListItem::ParentDir(_) => {}
            }
        }

        Ok(())
    }
}

enum KeyInputResult {
    Continue,
    Stop,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

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

fn handle_input_popup(app: &mut App, key: KeyEvent) -> KeyInputResult {
    match key.code {
        KeyCode::Char('q') => {
            app.sort_popup = false;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            app.dir_list.sort_by = SortBy::all()[app.dir_list.sort_by_list_state.selected().unwrap()].clone();
            app.sort_popup = false;
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

fn handle_input(app: &mut App, key: KeyEvent) -> KeyInputResult {
    if app.sort_popup {
        return handle_input_popup(app, key);
    }

    match key.code {
        KeyCode::Char('q') => {
            return KeyInputResult::Stop;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            // get the selected item
            if let Some(sel_idx) = app.dir_list.state.selected() {
                match &app.dir_list.items[sel_idx] {
                    DirectoryListItem::ParentDir(chg_dir) => {
                        app.navigate_to_relative_directory(chg_dir.to_owned()).ok();
                    }
                    DirectoryListItem::Entry(entry) => {
                        if entry.file_type().unwrap().is_dir() {
                            app.navigate_to_relative_directory(entry.file_name().into_string().unwrap()).ok();
                        } else {
                            opener::open(entry.path()).ok();
                        }
                    }
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.dir_list.select_next();
            app.load_file_snippet().ok();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.dir_list.select_previous();
            app.load_file_snippet().ok();
        }
        KeyCode::Left | KeyCode::Char('h') => {
            app.navigate_to_parent_directory().ok();
        }
        KeyCode::Char('s') => {
            app.sort_popup = !app.sort_popup;
        }
        _ => {}
    }

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
            if let Event::Key(key) = event::read()? {
                match handle_input(&mut app, key) {
                    KeyInputResult::Stop => {
                        return Ok(());
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
                DirectoryListItem::ParentDir(item) => {
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
                        style = link_style;
                    }
                    let mut filesize_str = "".to_string();
                    if item.file_type().unwrap().is_file() {
                        let file_size = item.metadata().unwrap().len();
                        let byte = Byte::from_bytes(file_size.into());
                        let adjusted_byte = byte.get_appropriate_unit(false);
                        filesize_str = adjusted_byte.to_string();
                    } else if item.file_type().unwrap().is_dir() {
                        // TODO: Two problems here...
                        // 1 - this should be async and populate value later (if slow)
                        // 2 - Avoid 'Operation not permitted', also seen with `du` on MacOS
                        let dir_size = get_size(item.path()).unwrap_or(0);
                        let byte = Byte::from_bytes(dir_size.into());
                        let adjusted_byte = byte.get_appropriate_unit(false);
                        filesize_str = adjusted_byte.to_string();
                    }
                    let meta = item.metadata().unwrap();
                    let uid = meta.st_uid();
                    let mut user = uid.to_string();
                    if let Some(uname) = get_user_by_uid(uid) {
                        user = uname.name().to_os_string().into_string().unwrap();
                    }
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
            Constraint::Length(20),     // name
            Constraint::Length(10),     // size
            Constraint::Length(12),     // user
            Constraint::Length(5),      // group
            Constraint::Length(3),      // usr (mask)
            Constraint::Length(3),      // grp (mask)
            Constraint::Length(3),      // oth (mask)
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

    if app.sort_popup {
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