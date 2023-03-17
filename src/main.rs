use std::{
    cmp::Ordering,
    error::Error,
    fs,
    fs::DirEntry,
    io,
    time::{Duration, Instant},
};
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

/// This struct holds the current state of the app. In particular, it has the `items` field which is a wrapper
/// around `ListState`. Keeping track of the items state let us render the associated widget with its state
/// and have access to features such as natural scrolling.
///
/// Check the event handling at the bottom to see how to change the state on incoming events.
/// Check the drawing logic for items on how to specify the highlighting style for selected items.
struct DirectoryList {
    state: ListState,
    items: Vec<DirEntry>,
}

impl DirectoryList {
    fn refresh(&mut self, dir: &str) {
        self.items.clear();
        self.items = fs::read_dir(dir).unwrap().into_iter().map(|x| x.unwrap()).collect();
        fs::read_dir(format!("{}/..", dir)).unwrap();
        self.items.sort_by(|a, b| DirectoryList::compare_dir_items(a, b));
    }

    fn compare_dir_items(a: &DirEntry, b: &DirEntry) -> Ordering {
        let a_file_type = a.file_type().unwrap();
        let b_file_type = b.file_type().unwrap();

        if a_file_type.is_dir() && !b_file_type.is_dir() {
            return Ordering::Less;
        } else if !a_file_type.is_dir() && b_file_type.is_dir() {
            return Ordering::Greater;
        } else {
            return a.file_name().cmp(&b.file_name());
        }
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
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
                state: ListState::default(),
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

    // Create ListItems for each directory entry
    let list_items: Vec<ListItem> = app.dir_list.items
        .iter()
        .map(|item| {
            // The item gets its own line
            let file_name = item.file_name().into_string().unwrap();
            let mut style = Style::default();
            if item.file_type().unwrap().is_dir() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if item.file_type().unwrap().is_symlink() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            let line = Spans::from(vec![Span::styled(file_name, style)]);

            ListItem::new(vec![line])
        })
        .collect();

    /*
    let parent_dir = Spans::from(vec![Span::styled("..", Style::default().add_modifier(Modifier::BOLD))]);
    list_items.insert(0, ListItem::new(vec![parent_dir]));
     */

    // Add the items to the list, highlighting
    let items_list = List::new(list_items)
        .block(Block::default().borders(Borders::ALL).title(app.dir.as_str()))
        .start_corner(Corner::TopLeft)
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(items_list, chunks[0], &mut app.dir_list.state);

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
