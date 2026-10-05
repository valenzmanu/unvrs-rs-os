//! Searchable command menus. Pages disclose choices before actions or forms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Form {
    Message(usize),
    Mission,
    Handoff(usize, &'static str),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Page {
    Root,
    Crew,
    Seat(usize),
    Handoff(usize),
    Missions,
    Logs,
    Compose(Form),
    Galaga,
    Display,
    Help,
}
#[derive(Clone, Copy)]
pub enum Action {
    Open(Page),
    Comms(usize),
    Obs,
    LogFilter(Option<&'static str>),
    Play,
    Motion,
    Admit(usize),
    Boot(usize),
    Launch,
    Picker,
    TestHandoffs,
    NewFlight,
    Cancel(usize),
    Recorder(usize),
    Quit,
}
pub struct Entry {
    pub label: String,
    pub detail: String,
    pub action: Action,
    pub enabled: bool,
}
impl Entry {
    pub fn new(label: impl Into<String>, detail: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            action,
            enabled: true,
        }
    }
}
pub struct Palette {
    pub page: Page,
    pub selected: usize,
    pub query: String,
    pub error: String,
    history: Vec<Page>,
}
impl Default for Palette {
    fn default() -> Self {
        Self {
            page: Page::Root,
            selected: 0,
            query: String::new(),
            error: String::new(),
            history: vec![],
        }
    }
}
impl Palette {
    pub fn open(&mut self, page: Page) {
        self.history.push(self.page);
        self.page = page;
        self.clear();
    }
    pub fn back(&mut self) -> bool {
        if let Some(page) = self.history.pop() {
            self.page = page;
            self.clear();
            true
        } else {
            false
        }
    }
    fn clear(&mut self) {
        self.query.clear();
        self.error.clear();
        self.selected = 0;
    }
    pub fn title(&self) -> String {
        match self.page {
            Page::Root => "COMMAND / BRIDGE".into(),
            Page::Crew => "BRIDGE › CREW".into(),
            Page::Seat(pid) => format!("CREW › PID {pid} › ACTIONS"),
            Page::Handoff(pid) => format!("PID {pid} › HANDOFF › HARNESS"),
            Page::Missions => "BRIDGE › MISSIONS · Enter admits".into(),
            Page::Logs => "BRIDGE › FLIGHT LOGS".into(),
            Page::Compose(Form::Message(pid)) => format!("PID {pid} › MESSAGE"),
            Page::Compose(Form::Mission) => "BRIDGE › NEW MISSION".into(),
            Page::Compose(Form::Handoff(pid, harness)) => {
                format!("PID {pid} › HANDOFF › {harness} › NEXT FOCUS")
            }
            Page::Galaga => "BRIDGE › GALAGA".into(),
            Page::Display => "BRIDGE › DISPLAY".into(),
            Page::Help => "BRIDGE › CONTROLS".into(),
        }
    }
    pub fn matches(&self, entry: &Entry) -> bool {
        let haystack = format!("{} {}", entry.label, entry.detail).to_lowercase();
        self.query
            .to_lowercase()
            .split_whitespace()
            .all(|word| haystack.contains(word))
    }
}
