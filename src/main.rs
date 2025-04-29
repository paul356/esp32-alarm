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

// Configuration for WiFi connection
const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");

// Time sync interval in seconds
const NTP_SYNC_INTERVAL: u64 = 3600; // 1 hour

// GPIO pin for the buzzer
const BUZZER_PIN: i32 = 5; // D5 on most ESP32 dev boards, adjust as needed

fn main() -> Result<()> {
    // Initialize ESP-IDF
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("ESP32 Alarm Clock starting...");
    
    // Get access to the peripherals
    let peripherals = Peripherals::take()?;
    
    // Set up buzzer on GPIO pin
    let pin = peripherals.pins.gpio5;
    let mut buzzer = PinDriver::output(pin)?;
    
    // Get the system event loop
    let sysloop = EspSystemEventLoop::take()?;
    
    // Connect to WiFi
    log::info!("Connecting to WiFi network '{}'...", SSID);
    let _wifi = connect_wifi(peripherals.modem, sysloop.clone(), SSID, PASSWORD)?;
    
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
    
    // Main loop
    loop {
        // Check if it's time to sync with NTP
        if let Ok(elapsed) = last_sync_time.elapsed() {
            if elapsed.as_secs() > NTP_SYNC_INTERVAL {
                log::info!("Performing scheduled NTP time sync");
                // Just recreate the SNTP client instead of calling update
                if let Ok(_) = setup_sntp() {
                    last_sync_time = SystemTime::now();
                }
            }
        }
        
        // Check if we've entered a new hour
        if let Ok(current_time) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            let now = current_time.as_secs();
            let secs = now % 60;
            let mins = (now / 60) % 60;
            let hours = (now / 3600) % 24;
            
            // Log current time every 5 minutes
            if mins % 5 == 0 && secs < 10 {
                log::info!("Current time: {:02}:{:02}", hours, mins);
            }
            
            // Sound alarm at the start of each hour
            if hours as i32 != last_hour && mins == 0 && secs < 10 {
                last_hour = hours as i32;
                log::info!("ALARM! It's now {}:00", hours);
                sound_alarm(&mut buzzer)?;
            }
        }
        
        thread::sleep(Duration::from_millis(500));
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
    let sntp = EspSntp::new_default()?;
    log::info!("SNTP initialized, waiting for time sync...");
    Ok(sntp)
}

// Sound the buzzer alarm
fn sound_alarm<T: OutputPin>(buzzer: &mut PinDriver<'_, T, Output>) -> Result<()> {
    // Sound pattern: 3 short beeps, pause, 3 short beeps
    for _ in 0..2 {
        for _ in 0..3 {
            buzzer.set_high()?;
            thread::sleep(Duration::from_millis(200));
            buzzer.set_low()?;
            thread::sleep(Duration::from_millis(200));
        }
        thread::sleep(Duration::from_millis(500));
    }
    
    Ok(())
}
