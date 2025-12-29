use clap::{Parser, ValueEnum};
use eros::{Context, ErrorUnion, ReshapeUnion, TE, Traced, UResult, Union};
use rusb::{Context as LibUsbContext, DeviceHandle, UsbContext};
use std::cmp::min;
use std::f64::consts::TAU;
use std::io::{self, Read};
use std::num::ParseIntError;
use std::{thread, time::Duration};

const INTERFACE: u8 = 3;

// Protocol Constants
const REQUEST_TYPE_WRITE: u8 = 0x21;
const REQUEST_TYPE_READ: u8 = 0xA1;
const REQUEST_SET_REPORT: u8 = 0x09;
const REQUEST_GET_REPORT: u8 = 0x01;
const VALUE_FEATURE_REPORT: u16 = 0x0300;
const INDEX_INTERFACE: u16 = 3;

const MAX_KEYS: usize = 120;
const CHUNK_SIZE: usize = 16;
const HEARTBEAT_INTERVAL: u64 = 64; // Sends "Enable" packet 

// ==========================================
// DRIVER LAYER
// ==========================================
struct Infi75<T: UsbContext> {
    handle: DeviceHandle<T>,
}

impl<T: UsbContext> Infi75<T> {
    fn new(context: &T, vid: u16, pid: u16) -> UResult<Self, (TE<rusb::Error>, TE)> {
        let device = context
            .devices()
            .traced()
            .context("Failed to fetch device list")
            .union()?
            .iter()
            .find_map(|d| {
                let Ok(desc) = d.device_descriptor() else {
                    return None;
                };
                if desc.vendor_id() == vid && desc.product_id() == pid {
                    Some(d)
                } else {
                    None
                }
            })
            .context("Usb device not found")
            .union()?;

        let handle = device.open().traced().union()?;

        // Detach kernel driver to prevent interference
        if handle.kernel_driver_active(INTERFACE).unwrap_or(false) {
            let _ = handle.detach_kernel_driver(INTERFACE);
        }

        handle
            .claim_interface(INTERFACE)
            .traced()
            .context("Failed to claim interface")
            .union()?;

        Ok(Self { handle })
    }

    /// Sends the "I'm still here" signal to reset the watchdog timer
    fn send_heartbeat(&self) -> Result<(), TE<rusb::Error>> {
        let mut buffer = [0u8; 64];
        buffer[0] = 0x04; // Command
        buffer[1] = 0x20; // Enable Music Mode
        self.send_packet(&buffer)
            .context("failed to send heartbeat command")
    }

    fn send_packet(&self, data: &[u8]) -> Result<(), TE<rusb::Error>> {
        let timeout = Duration::from_millis(100);
        self.handle
            .write_control(
                REQUEST_TYPE_WRITE,
                REQUEST_SET_REPORT,
                VALUE_FEATURE_REPORT,
                INDEX_INTERFACE,
                data,
                timeout,
            )
            .map(|_| ())
            .traced()
            .context("failed to enable write control")
    }

    /// Reads status to keep the USB pipe clean and prevent stalls
    fn drain_status(&self) {
        let mut buffer = [0u8; 64];
        let _ = self.handle.read_control(
            REQUEST_TYPE_READ,
            REQUEST_GET_REPORT,
            VALUE_FEATURE_REPORT,
            INDEX_INTERFACE,
            &mut buffer,
            Duration::from_millis(10),
        );
    }

    fn send_frame(&self, colors: &[(u8, u8, u8); MAX_KEYS]) -> Result<(), TE<rusb::Error>> {
        let mut key_index = 1;
        while key_index <= MAX_KEYS {
            let mut buffer = [0u8; 64];
            for i in 0..CHUNK_SIZE {
                if key_index > MAX_KEYS {
                    break;
                }
                let (r, g, b) = colors[key_index - 1];
                let pos = i * 4;
                buffer[pos] = key_index as u8;
                buffer[pos + 1] = r;
                buffer[pos + 2] = g;
                buffer[pos + 3] = b;
                key_index += 1;
            }

            self.send_packet(&buffer)
                .context("failed to send frame data")?;
            key_index += 1;
        }

        // Read status once per frame to sync with the device
        self.drain_status();

        Ok(())
    }
}

// ==========================================
// APPLICATION LOGIC
// ==========================================
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum Mode {
    Wave,
    Cava,
    Static,
}

fn parse_int_auto(s: &str) -> Result<u16, ParseIntError> {
    let pairs: [(&str, u32); 3] = [("0x", 16), ("0o", 8), ("0b", 2)];
    for (prefix, radix) in pairs {
        if let Some(v) = s.strip_prefix(prefix) {
            return u16::from_str_radix(v, radix);
        }
    }

    s.parse::<u16>()
}

#[derive(Parser)]
struct Args {
    #[arg(short, long, value_enum, default_value_t = Mode::Wave)]
    mode: Mode,
    #[arg(short, long, default_value_t = 1.0)]
    brightness: f32,
    #[arg(long, value_parser = parse_int_auto, default_value_t = 0x05ac)]
    vid: u16,
    #[arg(long, value_parser = parse_int_auto, default_value_t = 0x024f)]
    pid: u16,
}

fn apply_brightness(r: u8, g: u8, b: u8, factor: f32) -> (u8, u8, u8) {
    (
        (f32::from(r) * factor) as u8,
        (f32::from(g) * factor) as u8,
        (f32::from(b) * factor) as u8,
    )
}

fn main() -> UResult<(), (TE<rusb::Error>, TE<io::Error>, TE)> {
    let args = Args::parse();
    let context = LibUsbContext::new()
        .traced()
        .context("failed to start usb context")
        .union()?;

    loop {
        println!("Connecting to Infi75...");

        let keyboard = Infi75::new(&context, args.vid, args.pid).widen()?;

        // Initial Handshake
        if let Err(e) = keyboard.send_heartbeat() {
            eprintln!("Init Failed: {e}. Retrying...");
            continue;
        }

        println!("Music Mode Active. Starting {:?}...", args.mode);

        let mut frame = [(0u8, 0u8, 0u8); MAX_KEYS];

        // Run selected mode
        let err: ErrorUnion<(TE<rusb::Error>, TE<io::Error>, TE)> = match args.mode {
            Mode::Wave => run_wave(&keyboard, &mut frame, args.brightness).union(),
            Mode::Cava => run_cava(&keyboard, &mut frame, args.brightness).widen(),
            Mode::Static => run_static(&keyboard, &mut frame, args.brightness).union(),
        }
        .expect_err("unreachable");

        // If run_* returns, it means the connection broke.
        eprintln!("Connection lost due to {err:#?}. Rebooting driver...");
        // thread::sleep(Duration::from_secs(1));
    }
}

// --- WAVE EFFECT ---
fn run_wave<T: UsbContext>(
    kb: &Infi75<T>,
    frame: &mut [(u8, u8, u8); MAX_KEYS],
    brightness: f32,
) -> Result<(), TE<rusb::Error>> {
    let mut offset: f64 = 0.0;
    let mut frame_count = 0;

    loop {
        // Generate Rainbow
        for (i, key) in frame.iter_mut().enumerate() {
            let idx = i as f64;
            let r = idx.mul_add(0.1, offset).sin().mul_add(127.0, 128.0) as u8;
            let g = (idx.mul_add(0.1, offset) + 2.0).sin().mul_add(127.0, 128.0) as u8;
            let b = (idx.mul_add(0.1, offset) + 4.0).sin().mul_add(127.0, 128.0) as u8;
            *key = apply_brightness(r, g, b, brightness);
        }

        // Send Frame
        kb.send_frame(frame)?;

        // Heartbeat
        if frame_count % HEARTBEAT_INTERVAL == 0 {
            kb.send_heartbeat()?;
        }

        offset += 0.2;
        if offset > TAU {
            offset = 0.0;
        }
        frame_count += 1;
        thread::sleep(Duration::from_millis(30));
    }
}

// --- STATIC EFFECT ---
fn run_static<T: UsbContext>(
    kb: &Infi75<T>,
    frame: &mut [(u8, u8, u8); MAX_KEYS],
    brightness: f32,
) -> Result<(), TE<rusb::Error>> {
    let mut frame_count = 0;

    // Set color once
    frame.fill(apply_brightness(0, 255, 255, brightness)); // Cyan

    loop {
        kb.send_frame(frame)?;

        if frame_count % HEARTBEAT_INTERVAL == 0 {
            kb.send_heartbeat()?;
        }
        frame_count += 1;
        thread::sleep(Duration::from_millis(30));
    }
}

// --- CAVA AUDIO VISUALIZER ---
// Helper: Returns (Column, Row)
// Returns None if the key should be ignored (like F-Keys)
const fn get_vu_coords(key_idx: usize) -> Option<(usize, usize)> {
    match key_idx {
        0..=17 => None,
        18..=35 => Some((key_idx - 18, 0)),
        36..=48 => Some((key_idx - 36, 1)),
        52..=65 => Some((key_idx - 52, 2)),
        72..=82 => Some((key_idx - 72, 3)),
        100 => Some((12, 3)), // up
        83 => Some((11, 4)),  // shift
        90..=99 => Some((key_idx - 90, 4)),
        // => None,
        // 66..=70 => None,
        // 101..=128 => None,
        0.. => None,
    }
    // match key_idx {
    //     // Row 0: F-Keys -> IGNORE (Return None)
    //     0..=15 => None,

    //     // === VISUALIZATION START ===

    //     // Row 1: Number Row
    //     16..=30 => Some((key_idx - 16, 0)),

    //     // Row 2: QWERTY
    //     31..=39 => Some((key_idx - 31, 1)),

    //     // Row 4: ZXCV
    //     40..=50 => Some(((key_idx - 40), 3)),

    //     // Row 3: ASDF
    //     51..=73 => Some(((key_idx - 51), 2)),

    //     // Row 5: Space/Arrows
    //     74.. => Some(((key_idx - 74), 4)),
    // }
}

fn get_gradient_color(intensity: f32) -> (u8, u8, u8) {
    // intensity 0.0 to 1.0
    // Cold (Blue) -> Medium (Green) -> Hot (Red)
    if intensity < 0.5 {
        // Blue to Green
        let t = intensity * 2.0;
        (0, (255.0 * t) as u8, (255.0 * (1.0 - t)) as u8)
    } else {
        // Green to Red
        let t = (intensity - 0.5) * 2.0;
        ((255.0 * t) as u8, (255.0 * (1.0 - t)) as u8, 0)
    }
}

fn run_cava<T: UsbContext>(
    kb: &Infi75<T>,
    frame: &mut [(u8, u8, u8); MAX_KEYS],
    _global_brightness: f32,
) -> UResult<(), (TE<rusb::Error>, TE<io::Error>)> {
    const BAR_COUNT: usize = 16;

    let mut buffer = [0u8; BAR_COUNT];
    let mut smooth_buffer = [0.0f32; BAR_COUNT];

    let stdin = io::stdin();

    println!("Listening on STDIN");
    let mut frame_count = 0;

    loop {
        let value = stdin.lock().read_exact(&mut buffer);
        value
            .traced()
            .context("failed to read from stdin")
            .union()?;

        // 1. Physics & Smoothing
        for i in 0..BAR_COUNT {
            let raw_val = f32::from(buffer[i]);
            smooth_buffer[i] = raw_val;
        }

        // 2. Render Frame
        for (key_idx, key) in frame.iter_mut().enumerate() {
            if key_idx <= 15 {
                let amp = smooth_buffer[13];

                // Calculate the "Radius" of the pulse (0.0 to 8.5)
                // We divide by ~220.0 to hit max width before clipping for better visual impact
                let pulse_length = (amp / 220.0) * 15.;

                if amp < 10. {
                    *key = (0, 0, 0);
                    continue;
                }

                match key_idx {
                    0..2 => {
                        *key = apply_brightness(0, 255, 255, 0.5);
                    }
                    2..4 => {
                        *key = apply_brightness(255, 255, 0, 0.7);
                    }
                    4..7 => {
                        *key = apply_brightness(0, 255, 0, 1.0);
                    }
                    7..9 => {
                        *key = apply_brightness(255, 40, 40, 1.0);
                    }
                    9.. => {
                        *key = apply_brightness(255, 0, 0, 1.0);
                    }
                }

                // If the key is outside the current pulse length, turn it off
                if (key_idx as f32) > pulse_length {
                    *key = (0, 0, 0);
                }
                continue;
            }
            // ===========================================

            let Some((col, row)) = get_vu_coords(key_idx) else {
                *key = (0, 0, 0);
                continue;
            };

            // Map key to visual coordinates
            let col_idx = min(col, BAR_COUNT - 1);
            let amp = smooth_buffer[col_idx];
            // // === THRESHOLDS ===
            let threshold = match row {
                0 => 180.0, // Number Row
                1 => 140.0, // QWERTY
                2 => 90.0,  // ASDF
                3 => 60.0,  // ZXCV
                _ => 20.0,  // Space
            };

            // // Normalize current row threshold against max amp
            // let row_intensity = threshold / 255.;

            if amp <= threshold {
                *key = (0, 0, 0);
                continue;
            }

            let (r, g, b) = match row {
                0 => (0, 255, 255),
                1 => (255, 180, 180),
                2 => (255, 225, 0),
                3 => (0, 255, 0),
                _ => (255, 0, 0),
            };
            *key = apply_brightness(r, g, b, 1.0);
        }

        kb.send_frame(frame).union()?;

        if frame_count % HEARTBEAT_INTERVAL == 0 {
            kb.send_heartbeat().union()?;
        }

        frame_count += 1;
    }
}
