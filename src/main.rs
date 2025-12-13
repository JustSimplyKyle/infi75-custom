use clap::{Parser, ValueEnum};
use eros::{Context, ReshapeUnion, TE, Traced, UResult, Union, bail, traced};
use rusb::{Context as LibUsbContext, DeviceHandle, UsbContext};
use std::cmp::min;
use std::f64::consts::TAU;
use std::io::{self, Read};
use std::{thread, time::Duration};

// ==========================================
// CONFIGURATION
// ==========================================
const VID: u16 = 0x05ac; // Spoofed Apple ID
const PID: u16 = 0x024f;
const INTERFACE: u8 = 3;

// Protocol Constants
const REQUEST_TYPE_WRITE: u8 = 0x21;
const REQUEST_TYPE_READ: u8 = 0xA1;
const REQUEST_SET_REPORT: u8 = 0x09;
const REQUEST_GET_REPORT: u8 = 0x01;
const VALUE_FEATURE_REPORT: u16 = 0x0300;
const INDEX_INTERFACE: u16 = 3;

// Limits & Timing
const MAX_KEYS: usize = 103;
const CHUNK_SIZE: usize = 16;
const HEARTBEAT_INTERVAL: u64 = 16; // Send "Enable" packet every 16 frames

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

#[derive(Parser)]
struct Args {
    #[arg(short, long, value_enum, default_value_t = Mode::Wave)]
    mode: Mode,
    #[arg(short, long, default_value_t = 1.0)]
    brightness: f32,
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

        let keyboard = Infi75::new(&context, VID, PID).widen()?;

        // Initial Handshake
        if let Err(e) = keyboard.send_heartbeat() {
            eprintln!("Init Failed: {e}. Retrying...");
            continue;
        }
        println!("Music Mode Active. Starting {:?}...", args.mode);

        let mut frame = [(0u8, 0u8, 0u8); MAX_KEYS];

        // Run selected mode
        match args.mode {
            Mode::Wave => run_wave(&keyboard, &mut frame, args.brightness).union()?,
            Mode::Cava => run_cava(&keyboard, &mut frame, args.brightness).widen()?,
            Mode::Static => run_static(&keyboard, &mut frame, args.brightness).union()?,
        }

        // If run_* returns, it means the connection broke.
        eprintln!("Connection lost. Rebooting driver...");
        thread::sleep(Duration::from_secs(1));
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
        // Row 0: F-Keys -> IGNORE (Return None)
        0..=15 => None,

        // === VISUALIZATION START ===

        // Row 1: Number Row -> VISUAL ROW 0 (The Top / RED Zone)
        16..=30 => Some((key_idx - 16, 0)),

        // Row 2: QWERTY -> VISUAL ROW 1 (Green)
        31..=45 => Some((key_idx - 31, 1)),

        // Row 3: ASDF -> VISUAL ROW 2 (Green)
        46..=59 => Some(((key_idx - 46) + 1, 2)),

        // Row 4: ZXCV -> VISUAL ROW 3 (Green)
        60..=73 => Some(((key_idx - 60) + 1, 3)),

        // Row 5: Space/Arrows -> VISUAL ROW 4 (Green/Base)
        74.. => Some(((key_idx - 74) + 2, 4)),
    }
}

fn run_cava<T: UsbContext>(
    kb: &Infi75<T>,
    frame: &mut [(u8, u8, u8); MAX_KEYS],
    _global_brightness: f32,
) -> UResult<(), (TE<rusb::Error>, TE<io::Error>)> {
    const BAR_COUNT: usize = 16;
    const SMOOTHING: f32 = 0.4;

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
            if raw_val > smooth_buffer[i] {
                smooth_buffer[i] = raw_val;
            } else {
                smooth_buffer[i] -= (smooth_buffer[i] - raw_val) * (1.0 - SMOOTHING);
            }
        }

        // 2. Render Frame
        for (key_idx, key) in frame.iter_mut().enumerate() {
            let Some((col, row)) = get_vu_coords(key_idx) else {
                // If it's an F-Key (None), turn it off and skip logic
                *key = (0, 0, 0);
                continue;
            };

            // Map key to visual coordinates
            let col_idx = min(col, BAR_COUNT - 1);
            let amp = smooth_buffer[col_idx];

            // === THRESHOLDS ===
            // We now have 5 Visual Rows (0=Top/Numbers, 4=Bottom/Space)
            // The thresholds determine how much volume is needed to light up that row.
            let threshold = match row {
                0 => 180.0, // Number Row (RED) - Needs loud volume
                1 => 140.0, // QWERTY (Green)
                2 => 90.0,  // ASDF
                3 => 60.0,  // ZXCV
                _ => 20.0,  // Space (Always on if sound exists)
            };

            if amp <= threshold {
                // Background (Off)
                *key = (0, 0, 0);
                continue;
            }

            // === COLOR ASSIGNMENT ===
            let (r, g, b) = match row {
                0 => (255, 0, 0),   // Top Row (Numbers) -> RED
                1 => (255, 125, 0), // 2nd Row (QWERTY)  -> GREEN
                2 => (0, 124, 0),   // 2nd Row (QWERTY)  -> GREEN
                3 => (0, 51, 8),    // 2nd Row (QWERTY)  -> GREEN
                _ => (0, 50, 16),   // Lower Rows -> GREEN (slightly teal for style)
            };

            // Fade-in brightness
            let over_threshold = amp - threshold;
            let brightness = (over_threshold / 40.0).clamp(0.2, 1.0);

            *key = apply_brightness(r, g, b, brightness);
        }

        kb.send_frame(frame).union()?;

        if frame_count % HEARTBEAT_INTERVAL == 0 {
            kb.send_heartbeat().union()?;
        }
        frame_count += 1;
    }
}
