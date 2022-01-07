use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    error::{Error},
    io::{self, Write},
    time::{Duration}
};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{BarChart, Block, Borders},
    Frame, Terminal,
};
use rand::{self, Rng, thread_rng};
use std::sync::mpsc::{channel, Receiver, Sender};
use chrono::{Local, prelude::*};
use serial::{self, unix::TTYPort, SerialPort};
use std::io::{BufWriter, Read};
use std::fs::{File, OpenOptions};

struct MeterData {
    time: DateTime<Local>,
    watt: u64,
    total: [u64; 2],
}

struct App {
    data: Vec<(String, u64, u64)>,
    meter_value: Receiver<MeterData>,
    log: BufWriter<File>,
}

fn demp_thread(tx: Sender<MeterData>) {
    let mut rng = thread_rng();
    let mut total: u64 = 0;

    loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let watt = rng.gen_range(0..2000);
            total += watt;
            
            let data = MeterData {
                time: Local::now(),
                watt,
                total: [total, 0],
            };

        match tx.send(data) {
            Ok(_) => (),
            Err(e) => {
                println!("Serial error {:?}", e);
                break;            
            },
        }
    }
}

fn serial_thread(port: TTYPort, tx: Sender<MeterData>) {
    let reader = dsmr5::Reader::new(port.bytes().map(|b| b.unwrap()));

    for readout in reader {
        let telegram = readout.to_telegram().unwrap();
        let state = dsmr5::Result::<dsmr5::state::State>::from(&telegram).unwrap();
    
        let mt = state.datetime.unwrap();
    
        let t = chrono::Local.ymd(2000 + mt.year as i32, mt.month as u32, mt.day as u32)
                .and_hms(mt.hour as u32, mt.minute as u32, mt.second as u32);

        let data = MeterData {
            time: t,
            watt: (state.power_delivered.unwrap() * 1000.0) as u64,
            total: [ (state.meterreadings[0].to.unwrap()) as u64, (state.meterreadings[1].to.unwrap()) as u64],
            //total: 0, 
        };

        match tx.send(data) {
            Ok(_) => (),
            Err(e) => {
                println!("Serial error {:?}", e);
                break;            
            },
        }
    }
}

const AVG_SAMPLES: usize = 20;

impl App {
    fn new(rx: Receiver<MeterData>, log: BufWriter<File>) -> App {
        App {
            data: Vec::with_capacity(AVG_SAMPLES),
            meter_value: rx,
            log,
        }
    }

    fn on_tick(&mut self, data: MeterData) {
        // Handle label
        if self.data.len() == AVG_SAMPLES {
            self.data.pop().unwrap();
        }

        let avg_len = std::cmp::min(self.data.len(), AVG_SAMPLES);

        let avg_xsec = {
            let mut avg = data.watt;

            for (_s, data, _avg) in &self.data[0..avg_len] {
                avg += data;
            }
            avg / (avg_len + 1) as u64           
        };

        let mut logstr = String::with_capacity(255);

        logstr.push_str(data.time.format("%Y-%m-%d %H:%M:%S").to_string().as_str());
        logstr.push_str(format!(";{};{};{};{};\n", data.total[0], data.total[1], data.watt, avg_xsec).as_str());
        self.log.write_all(logstr.as_bytes()).unwrap();

        let t = data.time.format("%H%M%S").to_string();
        self.data.insert(0, (t, data.watt, avg_xsec))
    }
}

const SETTINGS: serial::PortSettings = serial::PortSettings {
    baud_rate:    serial::Baud115200,
    char_size:    serial::Bits8,
    parity:       serial::ParityNone,
    stop_bits:    serial::Stop1,
    flow_control: serial::FlowNone,
};

fn main() -> Result<(), Box<dyn Error>> {

    let mut args = std::env::args();

    let (tx, rx) = channel::<MeterData>();

    let f = OpenOptions::new().write(true).create(true).append(true).open("log.csv")?;
    let mut logfile = BufWriter::new(f);

    let header = "TIME;NORMAAL [kW];DAL [kW];POWER [W];AVG [W];\n";
    logfile.write_all(header.as_bytes())?;

    let _id = if let Some(path) = args.nth(1) {
        let mut port = serial::open(&path)?;
        port.configure(&SETTINGS)?;
        // if let Err(e) = port.configure(&SETTINGS) {
        //     println!("Can't setup port: {:?}", e);
        //     return Ok(());
        // }
        port.set_timeout(std::time::Duration::from_secs(3))?;

        std::thread::spawn( || serial_thread(port, tx))
    } else {
        std::thread::spawn( || demp_thread(tx))
    };

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let app = App::new(rx, logfile);
    let res = run_app(&mut terminal, app);

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
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &app))?;

        if crossterm::event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') = key.code {
                    return Ok(());
                }
            }
        }
        match app.meter_value.recv_timeout(Duration::from_millis(3000)) {
            Ok(data) => app.on_tick(data),
            Err(e) => {
                return Err(io::Error::new(io::ErrorKind::Other, format!("RX channel: {:?}: Quit!", e)));
            },
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(f.size());

    let data_cur = app.data.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<(&str, u64)>>();   
    
    let barchart = BarChart::default()
        .block(Block::default().title("Current Watt").borders(Borders::ALL))
        .data(&data_cur)
        .bar_width(7)
        .bar_style(Style::default().fg(Color::Yellow))
        .value_style(Style::default().fg(Color::Black).bg(Color::Yellow));
    f.render_widget(barchart, chunks[0]);

    let data_cur = app.data.iter().map(|f| (f.0.as_str(), f.2)).collect::<Vec<(&str, u64)>>();   

    let barchart = BarChart::default()
        .block(Block::default().title("AVG x sampels").borders(Borders::ALL))
        .data(&data_cur)
        .bar_width(7)
        .bar_style(Style::default().fg(Color::Green))
        .value_style(Style::default().fg(Color::Black).bg(Color::Yellow));
    f.render_widget(barchart, chunks[1]);
}
