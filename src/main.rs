use anyhow::Result;
use esp_idf_svc::hal as hal;
use hal::gpio::{Output, PinDriver, OutputPin};
use hal::peripherals::Peripherals;
use hal::peripheral::Peripheral;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use esp_idf_svc::wifi::{ClientConfiguration, Configuration};
use std::time::{Duration, SystemTime};
use std::thread;
use std::sync::mpsc::{self, Receiver};

// Configuration for WiFi connection
const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");

// Time sync interval in seconds
const NTP_SYNC_INTERVAL: u64 = 3600; // 1 hour

// WiFi check interval in milliseconds
const WIFI_CHECK_INTERVAL: u64 = 30000; // 30 seconds

// Alarm pattern parameters
const BEEP_COUNT: u8 = 1; // Changed from 3 to 1
const BEEP_DURATION_MS: u64 = 200;
const BEEP_PAUSE_MS: u64 = 200;
const PATTERN_PAUSE_MS: u64 = 500;

// Message types for buzzer control - updated with parameters
enum BuzzerMessage {
    PlayAlarm {
        repeat_count: u8,
        frequency: u32,
    },
}

fn main() -> Result<()> {
    // Initialize ESP-IDF
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("ESP32 Alarm Clock starting...");

    // Get access to the peripherals
    let peripherals = Peripherals::take()?;

    // Get the system event loop
    let sysloop = EspSystemEventLoop::take()?;

    // Setup buzzer control channel and thread
    let (buzzer_tx, buzzer_rx) = mpsc::channel();

    // Start buzzer control thread
    thread::spawn(move || {
        let pin = peripherals.pins.gpio5;
        if let Ok(mut buzzer) = PinDriver::output(pin) {
            buzzer_control_task(buzzer_rx, &mut buzzer);
        } else {
            log::error!("Failed to initialize buzzer pin!");
        }
    });

    // Connect to WiFi
    log::info!("Connecting to WiFi network '{}'...", SSID);
    let mut wifi = connect_wifi(peripherals.modem, sysloop.clone(), SSID, PASSWORD)?;

    // Configure SNTP for time synchronization
    log::info!("Setting up SNTP service...");
    let sntp = setup_sntp()?;

    // Wait for initial time synchronization
    log::info!("Waiting for initial time sync...");
    while sntp.get_sync_status() != SyncStatus::Completed {
        thread::sleep(Duration::from_millis(500));
    }
    log::info!("Initial time sync complete");

    let mut last_sync_time = SystemTime::now();
    let mut last_hour = -1;
    let mut last_10_min_alarm = -1;
    let mut last_wifi_check = SystemTime::now();
    let mut last_log_time: i64 = -1; // Track the last time we logged

    // Main loop
    loop {
        // Check WiFi status periodically
        if let Ok(elapsed) = last_wifi_check.elapsed() {
            if elapsed.as_secs() * 1000 > WIFI_CHECK_INTERVAL {
                if !wifi_is_connected(&wifi) {
                    log::warn!("WiFi connection lost. Attempting to reconnect...");
                    if let Err(e) = wifi.connect() {
                        log::error!("Failed to reconnect to WiFi: {:?}", e);
                    } else if let Err(e) = wifi.wait_netif_up() {
                        log::error!("Failed to get IP address: {:?}", e);
                    } else {
                        let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
                        log::info!("WiFi reconnected, IP: {}", ip_info.ip);
                    }
                } else {
                    log::debug!("WiFi connection is stable");
                }
                last_wifi_check = SystemTime::now();
            }
        }

        // Check if it's time to sync with NTP
        if let Ok(elapsed) = last_sync_time.elapsed() {
            if elapsed.as_secs() > NTP_SYNC_INTERVAL {
                log::info!("Performing scheduled NTP time sync");
                // Just recreate the SNTP client instead of calling update
                if let Ok(_) = setup_sntp() {
                    last_sync_time = SystemTime::now();
                    log::info!("Time sync completed");
                } else {
                    log::error!("Time sync failed");
                }
            }
        }

        // Check if we've entered a new hour
        if let Ok(current_time) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            let now = current_time.as_secs();
            let _secs = now % 60;  // Prefixed with underscore as it's now unused
            let mins = (now / 60) % 60;
            let hours = (now / 3600) % 24;

            // Log current time every 5 minutes but only once per interval
            let current_log_key = ((hours * 60 + mins) / 5) as i64; // Convert to i64 to match last_log_time
            if current_log_key != last_log_time && mins % 5 == 0 {
                log::info!("Current time: {:02}:{:02}", hours, mins);
                last_log_time = current_log_key;
            }

            // Only send alarms between 7:00 and 23:00
            let is_alarm_time = hours >= 7 && hours <= 23;

            // Sound alarm at the start of each hour
            if hours as i32 != last_hour && mins == 0 && is_alarm_time {
                last_hour = hours as i32;
                log::info!("ALARM! It's now {}:00", hours);

                // Send alarm message to buzzer thread
                // Set repeat count to the current hour and frequency to 2000Hz
                if let Err(e) = buzzer_tx.send(BuzzerMessage::PlayAlarm {
                    repeat_count: hours as u8,
                    frequency: 2000
                }) {
                    log::error!("Failed to send alarm to buzzer thread: {:?}", e);
                }
            }

            // Sound alarm at 10 minutes past each hour
            if hours as i32 != last_10_min_alarm && mins == 10 && is_alarm_time {
                last_10_min_alarm = hours as i32;
                log::info!("ALARM! It's now {}:10", hours);

                // Send alarm message to buzzer thread with repeat count 3 and frequency 2600Hz
                if let Err(e) = buzzer_tx.send(BuzzerMessage::PlayAlarm {
                    repeat_count: 3,
                    frequency: 2600
                }) {
                    log::error!("Failed to send 10-min alarm to buzzer thread: {:?}", e);
                }
            }
        }

        thread::sleep(Duration::from_millis(500));
    }
}

// Buzzer control task running in separate thread
fn buzzer_control_task<T: OutputPin>(
    receiver: Receiver<BuzzerMessage>,
    buzzer: &mut PinDriver<'_, T, Output>,
) {
    log::info!("Buzzer control thread started");

    loop {
        match receiver.recv() {
            Ok(BuzzerMessage::PlayAlarm { repeat_count, frequency }) => {
                log::debug!("Playing alarm pattern with {} repeats at {} Hz", repeat_count, frequency);
                if let Err(e) = play_alarm_pattern(buzzer, repeat_count, frequency) {
                    log::error!("Error playing alarm: {:?}", e);
                }
            },
            Err(e) => {
                log::error!("Error receiving message in buzzer thread: {:?}", e);
                // If channel is closed (e.g., main thread died), exit the thread
                break;
            }
        }
    }

    log::info!("Buzzer control thread exiting");
}

// Play the alarm pattern with the given frequency
fn play_alarm_pattern<T: OutputPin>(
    buzzer: &mut PinDriver<'_, T, Output>,
    repeat_count: u8,
    frequency: u32,
) -> Result<()> {
    for _ in 0..repeat_count {
        for _ in 0..BEEP_COUNT {
            play_tone(buzzer, frequency, BEEP_DURATION_MS)?;
            thread::sleep(Duration::from_millis(BEEP_PAUSE_MS));
        }
        thread::sleep(Duration::from_millis(PATTERN_PAUSE_MS));
    }

    Ok(())
}

// Play a tone with the specified frequency and duration
fn play_tone<T: OutputPin>(
    buzzer: &mut PinDriver<'_, T, Output>,
    freq_hz: u32,
    duration_ms: u64,
) -> Result<()> {
    if freq_hz == 0 {
        // If frequency is 0, just turn on for the duration
        buzzer.set_high()?;
        thread::sleep(Duration::from_millis(duration_ms));
        buzzer.set_low()?;
        return Ok(());
    }

    // Calculate half-period in microseconds
    let half_period_us: u64 = 500_000 / freq_hz as u64;
    let start = SystemTime::now();
    let duration_us = duration_ms * 1000;

    // Threshold below which we'll use a spin loop instead of sleep
    // FreeRTOS tick rate typically doesn't allow sleeps below 1ms (1000us)
    const MIN_SLEEP_THRESHOLD_US: u64 = 1000;

    let elapsed_us = || {
        SystemTime::now()
            .duration_since(start)
            .unwrap_or(Duration::from_secs(0))
            .as_micros() as u64
    };

    // Generate waveform for the specified duration
    while elapsed_us() < duration_us {
        buzzer.set_high()?;

        if half_period_us >= MIN_SLEEP_THRESHOLD_US {
            // For longer periods, sleep is efficient enough
            thread::sleep(Duration::from_micros(half_period_us));
        } else {
            // For shorter periods, use a spin loop for better precision
            let target = elapsed_us() + half_period_us;
            while elapsed_us() < target {
                // Busy wait (spin)
            }
        }

        buzzer.set_low()?;

        if half_period_us >= MIN_SLEEP_THRESHOLD_US {
            thread::sleep(Duration::from_micros(half_period_us));
        } else {
            let target = elapsed_us() + half_period_us;
            while elapsed_us() < target {
                // Busy wait (spin)
            }
        }
    }

    Ok(())
}

// Check if WiFi is still connected
fn wifi_is_connected<'a>(wifi: &BlockingWifi<EspWifi<'a>>) -> bool {
    match wifi.wifi().is_connected() {
        Ok(connected) => connected,
        Err(_) => false,
    }
}

// Connect to WiFi network
fn connect_wifi(
    modem: impl Peripheral<P = hal::modem::Modem> + 'static,
    sysloop: EspSystemEventLoop,
    ssid: &str,
    password: &str
) -> Result<BlockingWifi<EspWifi<'static>>> {
    let nvs = EspDefaultNvsPartition::take()?;

    // Create WiFi driver with the network interface
    let wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(wifi, sysloop)?;

    // Create WiFi configuration
    let wifi_configuration = Configuration::Client(ClientConfiguration {
        ssid: heapless::String::try_from(ssid).unwrap_or_default(),
        password: heapless::String::try_from(password).unwrap_or_default(),
        ..Default::default()
    });

    wifi.set_configuration(&wifi_configuration)?;
    wifi.start()?;

    log::info!("WiFi started, connecting...");

    wifi.connect()?;

    log::info!("Waiting for DHCP lease...");
    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    log::info!("WiFi connected, IP: {}", ip_info.ip);

    Ok(wifi)
}

// Setup SNTP service for time synchronization
fn setup_sntp() -> Result<EspSntp<'static>> {
    // Set timezone to UTC+8
    // For UTC+8: "CST-8" (China Standard Time, 8 hours ahead of UTC)
    let tz = std::ffi::CString::new("CST-8").unwrap();
    unsafe {
        esp_idf_svc::sys::setenv(
            std::ffi::CString::new("TZ").unwrap().as_ptr(),
            tz.as_ptr(),
            1
        );
        esp_idf_svc::sys::tzset();
    }

    log::info!("Timezone set to UTC+8 (CST)");

    let sntp = EspSntp::new_default()?;
    log::info!("SNTP initialized, waiting for time sync...");
    Ok(sntp)
}
