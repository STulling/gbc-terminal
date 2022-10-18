use std::io::{Write, Read};
use std::{io, u8};
use std::path::PathBuf;
use std::time::{Instant, Duration};

use gbc::Gameboy;
use gbc::cartridge::Cartridge;
use gbc::joypad::{JoypadEvent, JoypadInput};
use gbc::ppu::{FrameBuffer, LCD_WIDTH, LCD_HEIGHT};

use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::{thread};
use structopt::StructOpt;

pub use crossterm::{
    cursor,
    style,
    style::Color,
    event::{self, Event, KeyCode, KeyEvent},
    execute, queue,
    terminal::{self, ClearType},
    Command, Result,
};

const FRAMES_PER_CYCLE: u32 = 2;

#[derive(Debug, StructOpt)]
#[structopt(about = "A simple GBC terminal emulator written in Rust")]
enum Args {
    #[structopt(about = "Run a ROM on the emulator")]
    Run {
        #[structopt(parse(from_os_str), help = "Path to ROM file")]
        rom_file: PathBuf,
    }
}

fn char_to_joypad_input(keycode: Option<char>) -> Option<JoypadInput> {
    match keycode.unwrap() {
        'n' => Some(JoypadInput::B),
        'm' => Some(JoypadInput::A),
        'j' => Some(JoypadInput::Start),
        'k' => Some(JoypadInput::Select),
        'w' => Some(JoypadInput::Up),
        's' => Some(JoypadInput::Down),
        'a' => Some(JoypadInput::Left),
        'd' => Some(JoypadInput::Right),
        _ => None,
    }
}

pub fn read_char() -> Result<char> {
    let mut buf = [0u8; 1];
    io::stdin().read(&mut buf)?;
    Ok(buf[0] as char)
}

fn spawn_stdin_channel() -> Receiver<char> {
    let (tx, rx) = mpsc::channel::<char>();
    thread::spawn(move || loop {
        let c = read_char().unwrap();
        tx.send(c).unwrap();
    });
    rx
}


fn create_frame(frame_buffer: &FrameBuffer, frame: &mut Vec<u8>) {
    // Separate pixel into top and bottom color
    let mut prev_bg_color = Color::Rgb{r:0, g:0, b:0};
    let mut prev_fg_color = Color::Rgb{r:0, g:0, b:0};
    queue!(frame, style::SetBackgroundColor(prev_bg_color)).unwrap();
    queue!(frame, style::SetForegroundColor(prev_fg_color)).unwrap();

    for y in 0..LCD_HEIGHT/2 {
        for x in 0..LCD_WIDTH {
            let bg_color_vals = frame_buffer.read(x, y*2);
            let fg_color_vals = frame_buffer.read(x, y*2+1);
            let bg_color = Color::Rgb{r:bg_color_vals.red, g:bg_color_vals.green, b:bg_color_vals.blue};
            let fg_color = Color::Rgb{r:fg_color_vals.red, g:fg_color_vals.green, b:fg_color_vals.blue};
            if bg_color != prev_bg_color {
                queue!(frame, style::SetBackgroundColor(bg_color)).unwrap();
                prev_bg_color = bg_color;
            }
            if fg_color != prev_fg_color {
                queue!(frame, style::SetForegroundColor(fg_color)).unwrap();
                prev_fg_color = fg_color;
            }
            queue!(frame, style::Print("▄")).unwrap();
        }
        queue!(frame, cursor::MoveToNextLine(1)).unwrap();
    }
}

/// Renders a single Gameboy frame to the console
fn render_frame(frame_buffer: &FrameBuffer, frame: &mut Vec<u8>, stdout: &mut io::Stdout){
    // lock stdout
    let mut stdout = stdout.lock();
    // Clear the screen with crossterm
    queue!(
        stdout,
        style::ResetColor,
        //terminal::Clear(ClearType::All),
        cursor::MoveTo(0, 0)
    ).unwrap();
    // Render the frame
    create_frame(frame_buffer, frame);
    // Write the frame to stdout
    stdout.write_all(&frame).unwrap();
    // Flush the output
    stdout.flush().unwrap();
    // empty the frame buffer
    frame.clear();
}   

/// Handles a single Gameboy frame.
fn handle_frame(gameboy: &mut Gameboy, joypad_events: &mut Vec<JoypadEvent>, frame: &mut Vec<u8>, stdout: &mut io::Stdout) {
    for _ in 0..FRAMES_PER_CYCLE-1{
        gameboy.frame(Some(joypad_events));
    }

    let frame_buffer = gameboy.frame(Some(joypad_events));

    // Clear out all processed input events
    joypad_events.clear();

    // Render the frame
    render_frame(frame_buffer, frame, stdout);
}

fn cli(rom_file: PathBuf) -> Result<()> {

    let mut stdout = io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    // Hide the cursor
    execute!(stdout, cursor::Hide)?;

    // Load the ROM
    let cartridge = get_cartridge(&rom_file, false);

    // Create the Gameboy
    let mut gameboy = Gameboy::init(cartridge, false).unwrap();

    // Create a channel for receiving input from stdin
    let rx = spawn_stdin_channel();

    // Create a vector for storing input events
    let mut joypad_events = Vec::new();
    let mut pressed_keys = Vec::new();
    let mut previous_pressed_keys = Vec::new();

    // More accurate sleep, especially on Windows
    let sleeper = spin_sleep::SpinSleeper::default();

    let frame_time_ns = Gameboy::FRAME_DURATION * FRAMES_PER_CYCLE as u64;
    let frame_duration = Duration::from_nanos(frame_time_ns);

    let mut frame = Vec::with_capacity(LCD_HEIGHT * LCD_WIDTH * 16);

    // Start the event loop
    'running: loop {
        let frame_start = Instant::now();

        // See previous state of the joypad_events
        //let previous_joypad_events = joypad_events.clone();

        // Handle input
        loop {
            match rx.try_recv() {
                // Escape to quit
                Ok('q') => {
                    // leave alternate screen
                    execute!(stdout, terminal::LeaveAlternateScreen)?;
                    execute!(stdout, cursor::Show)?;
                    break 'running;
                }
                Ok(keycode) => {
                    pressed_keys.push(keycode);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'running,
            }
        }

        // Set the 'Up' events
        // This happens if the key was pressed in the previous frame, but not in this one
        for keycode in previous_pressed_keys.iter() {
            if !pressed_keys.contains(keycode) {
                if let Some(joypad_input) = char_to_joypad_input(Some(*keycode)) {
                    joypad_events.push(JoypadEvent::Up(joypad_input));
                }
            }
        }
        // Set the 'Down' events
        // This happens if the key was pressed in this frame, but not in the previous one
        for keycode in pressed_keys.iter() {
            if !previous_pressed_keys.contains(keycode) {
                if let Some(joypad_input) = char_to_joypad_input(Some(*keycode)) {
                    joypad_events.push(JoypadEvent::Down(joypad_input));
                }
            }
        }

        previous_pressed_keys = pressed_keys.clone();
        pressed_keys.clear();

        handle_frame(&mut gameboy, &mut joypad_events, &mut frame, &mut stdout);

        let elapsed = frame_start.elapsed();

        //log::debug!("Frame time: {:?}", elapsed);

        // Sleep for the rest of the frame
        //
        // TODO: Evaluate if we need VSYNC to avoid tearing on higher Hz displays
        if elapsed < frame_duration {
            sleeper.sleep(frame_duration - elapsed);
        }
    }
    Ok(())
}

fn get_cartridge(path: &PathBuf, boot_rom: bool) -> Cartridge {
    let data = std::fs::read(path).expect("Failed to open ROM file");
    let cartridge = Cartridge::from_bytes(data, boot_rom);
    cartridge
}

fn main(){
    env_logger::init();

    let cli2 = Args::from_args();

    match cli2 {
        Args::Run { rom_file,} => {
            cli(rom_file).unwrap();
        }
    }
}
