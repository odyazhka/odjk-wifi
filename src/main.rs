#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use once_cell::sync::Lazy;

// ─── Lang ─────────────────────────────────────────────────────────────────────

struct Lang {
    title: &'static str,
    sudo_prompt: &'static str,
    sudo_placeholder: &'static str,
    unlock: &'static str,
    scanning: &'static str,
    refresh: &'static str,
    no_networks: &'static str,
    connect: &'static str,
    password_prompt: &'static str,
    password_placeholder: &'static str,
    cancel: &'static str,
    connecting: &'static str,
    connected_ok: &'static str,
    connected_fail: &'static str,
    wrong_sudo: &'static str,
    open_network: &'static str,
    show_pw: &'static str,
    close: &'static str,
    saved_pw_hint: &'static str,
    wait_hint: &'static str,
}

const RU: Lang = Lang {
    title: "Wi-Fi менеджер",
    sudo_prompt: "Введите пароль sudo для управления Wi-Fi:",
    sudo_placeholder: "пароль sudo…",
    unlock: "Разблокировать",
    scanning: "Сканирование…",
    refresh: "↻  Обновить",
    no_networks: "Сети не найдены. Нажмите «Обновить».",
    connect: "Подключиться",
    password_prompt: "Введите пароль Wi-Fi сети:",
    password_placeholder: "пароль сети…",
    cancel: "Отмена",
    connecting: "Подключение…",
    connected_ok: "✔ Подключено успешно!",
    connected_fail: "❌ Ошибка подключения (неверный пароль или слабый сигнал).",
    wrong_sudo: "Неверный пароль sudo.",
    open_network: "(открытая)",
    show_pw: "Показать пароль",
    close: "Закрыть",
    saved_pw_hint: "🔑 Используется сохранённый пароль",
    wait_hint: "Подождите, проверяется соединение…",
};

const EN: Lang = Lang {
    title: "Wi-Fi Manager",
    sudo_prompt: "Enter your sudo password to manage Wi-Fi:",
    sudo_placeholder: "sudo password…",
    unlock: "Unlock",
    scanning: "Scanning…",
    refresh: "↻  Refresh",
    no_networks: "No networks found. Press «Refresh».",
    connect: "Connect",
    password_prompt: "Enter the Wi-Fi password:",
    password_placeholder: "network password…",
    cancel: "Cancel",
    connecting: "Connecting…",
    connected_ok: "✔ Connected successfully!",
    connected_fail: "❌ Connection failed (wrong password or weak signal).",
    wrong_sudo: "Wrong sudo password.",
    open_network: "(open)",
    show_pw: "Show password",
    close: "Close",
    saved_pw_hint: "🔑 Using saved password",
    wait_hint: "Please wait, verifying connection…",
};

fn detect_lang() -> &'static Lang {
    for var in &["LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES"] {
        if let Ok(val) = std::env::var(var) {
            if val.to_lowercase().starts_with("ru") {
                return &RU;
            }
        }
    }
    &EN
}

// ─── Signal bars (pure Unicode, no Nerd Font needed) ─────────────────────────

fn signal_bars(dbm: i32) -> (&'static str, egui::Color32) {
    match dbm {
        s if s >= -50 => ("▂▄▆█", egui::Color32::from_rgb(80, 210, 80)),
        s if s >= -60 => ("▂▄▆_", egui::Color32::from_rgb(150, 220, 80)),
        s if s >= -70 => ("▂▄__", egui::Color32::from_rgb(220, 200, 60)),
        s if s >= -80 => ("▂___", egui::Color32::from_rgb(220, 130, 40)),
        _             => ("____", egui::Color32::from_rgb(180, 60, 60)),
    }
}

// ─── password.wifi ────────────────────────────────────────────────────────────
//
// Fixed path: always lives next to the release binary so the file survives
// rebuilds and is easy to find.
// Full path: /home/wan/hello_void/target/x86_64-unknown-linux-musl/release/password.wifi
  
    
static PW_FILE_PATH: Lazy<PathBuf> = Lazy::new(|| {
    // Получаем путь к домашней папке пользователя
    let mut path = std::env::home_dir().expect("Не удалось найти домашнюю директорию");
    
    // Создаем директорию, если её еще нет
    let _ = std::fs::create_dir_all(&path);
    
    path.push("password.wifi");
    path
});

fn pw_file_path() -> PathBuf {
    // Теперь PW_FILE_PATH — это Lazy<PathBuf>, поэтому clone() вернет PathBuf
    PW_FILE_PATH.clone()
}

/// Read all saved credentials from password.wifi.
///
/// File format — one entry per line:
///   `SSID<TAB>PASSWORD\n`
///
/// We split only on the **first** tab, so passwords that happen to contain
/// a tab character are stored and retrieved correctly.
fn read_pw_file() -> std::collections::HashMap<String, String> {
	let mut map = std::collections::HashMap::new();
    let path = pw_file_path(); // Получаем динамический путь
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return map,
    };
    for line in content.lines() {
        if let Some((ssid, pw)) = line.split_once('\t') {
            let ssid = ssid.trim();
            if !ssid.is_empty() {
                map.insert(ssid.to_string(), pw.to_string());
            }
        }
    }
    map
}

/// Persist a verified SSID+password pair into password.wifi.
///
/// * Creates the parent directory tree if it does not exist.
/// * Updates an existing entry for the same SSID (no duplicates).
/// * Writes atomically-ish: builds the full content in memory first,
///   then does a single `fs::write` call.
fn save_pw_file(ssid: &str, password: &str) {
    let path = pw_file_path();

    // Make sure the directory exists (it should — it's the release dir —
    // but create_dir_all is a no-op when it already does).
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("[password.wifi] cannot create directory {:?}: {}", parent, e);
            return;
        }
    }

    // Merge with existing entries so we don't lose other saved networks.
    let mut map = read_pw_file();
    map.insert(ssid.to_string(), password.to_string());

    // Serialise: SSID TAB PASSWORD NEWLINE
    let mut content = String::new();
    for (s, p) in &map {
        content.push_str(s);
        content.push('\t');
        content.push_str(p);
        content.push('\n');
    }

    match std::fs::write(&path, content.as_bytes()) {
        Ok(_) => eprintln!("[password.wifi] saved credentials for \"{}\" to {:?}", ssid, path),
        Err(e) => eprintln!("[password.wifi] write failed for {:?}: {}", path, e),
    }
}

/// Return the saved password for `ssid`, or `None` if not found.
fn lookup_saved_pw(ssid: &str) -> Option<String> {
    read_pw_file().remove(ssid)
}

// ─── Network ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Network {
    ssid: String,
    signal: i32,
    flags: String,
}

impl Network {
    fn is_open(&self) -> bool {
        !self.flags.contains("WPA") && !self.flags.contains("WEP")
    }
}

// ─── Shared state ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct ScanState {
    networks: Vec<Network>,
    scanning: bool,
    connected_ssid: String,
}

// ─── Connect dialog ───────────────────────────────────────────────────────────

struct ConnectDialog {
    ssid: String,
    is_open: bool,
    password: String,
    show_password: bool,
    connecting: bool,
    result: Arc<Mutex<Option<String>>>,
    /// pre-filled from password.wifi, shown as placeholder hint
    saved_password: Option<String>,
}

// ─── App ──────────────────────────────────────────────────────────────────────

enum Screen {
    SudoAuth,
    Main,
}

struct WifiApp {
    lang: &'static Lang,
    screen: Screen,
    sudo_password: String,
    sudo_error: Option<String>,
    scan: Arc<Mutex<ScanState>>,
    last_auto_scan: Option<Instant>,
    dialog: Option<ConnectDialog>,
}

// ─── sudo helpers ─────────────────────────────────────────────────────────────

fn sudo_run(sudo_pw: &str, args: &[&str]) -> bool {
    Command::new("sudo")
        .arg("-S")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut c| {
            if let Some(s) = c.stdin.as_mut() {
                let _ = s.write_all(format!("{}\n", sudo_pw).as_bytes());
            }
            c.wait()
        })
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sudo_output(sudo_pw: &str, args: &[&str]) -> String {
    Command::new("sudo")
        .arg("-S")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut c| {
            if let Some(s) = c.stdin.as_mut() {
                let _ = s.write_all(format!("{}\n", sudo_pw).as_bytes());
            }
            c.wait_with_output()
        })
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

// ─── wpa_cli helpers ──────────────────────────────────────────────────────────

fn fetch_networks(sudo_pw: &str) -> Vec<Network> {
    let raw = sudo_output(sudo_pw, &["wpa_cli", "scan_results"]);
    let mut map: std::collections::HashMap<String, Network> =
        std::collections::HashMap::new();
    for line in raw.lines().skip(1) {
        let cols: Vec<&str> = line.splitn(5, '\t').collect();
        if cols.len() < 5 {
            continue;
        }
        let ssid = cols[4].trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let signal: i32 = cols[2].trim().parse().unwrap_or(-100);
        let flags = cols[3].trim().to_string();
        map.entry(ssid.clone())
            .and_modify(|e| {
                if signal > e.signal {
                    e.signal = signal;
                    e.flags = flags.clone();
                }
            })
            .or_insert(Network { ssid, signal, flags });
    }
    let mut v: Vec<Network> = map.into_values().collect();
    v.sort_by(|a, b| b.signal.cmp(&a.signal));
    v
}

fn fetch_connected(sudo_pw: &str) -> String {
    let raw = sudo_output(sudo_pw, &["wpa_cli", "status"]);
    // Only report connected if wpa_state=COMPLETED
    let completed = raw.lines().any(|l| l.trim() == "wpa_state=COMPLETED");
    if !completed {
        return String::new();
    }
    for line in raw.lines() {
        if let Some(ssid) = line.strip_prefix("ssid=") {
            return ssid.trim().to_string();
        }
    }
    String::new()
}

/// Connect with proper wpa_cli sequence:
/// add_network → set ssid → set psk/key_mgmt → enable_network → select_network
/// Then poll status every 2s up to 30s total.
/// Returns (success, used_password).
/// Keeps the old network active if connection fails.
fn connect_network(
    ssid: &str,
    psk: &str,
    is_open: bool,
    sudo_pw: &str,
) -> (bool, String) {
    // 1. Remember which network is currently active so we can restore it
    let prev_status = sudo_output(sudo_pw, &["wpa_cli", "status"]);
    let prev_net_id: Option<String> = prev_status.lines().find_map(|l| {
        l.strip_prefix("id=").map(|v| v.trim().to_string())
    });

    // 2. add_network
    let net_id_raw = sudo_output(sudo_pw, &["wpa_cli", "add_network"]);
    let net_id = net_id_raw
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
        })
        .last()
        .unwrap_or("")
        .trim()
        .to_string();

    if net_id.is_empty() {
        return (false, psk.to_string());
    }

    // 3. set ssid
    sudo_run(
        sudo_pw,
        &["wpa_cli", "set_network", &net_id, "ssid", &format!("\"{}\"", ssid)],
    );

    // 4. set credentials
    if is_open {
        sudo_run(sudo_pw, &["wpa_cli", "set_network", &net_id, "key_mgmt", "NONE"]);
    } else {
        sudo_run(
            sudo_pw,
            &["wpa_cli", "set_network", &net_id, "psk", &format!("\"{}\"", psk)],
        );
    }

    // 5. enable + select (this may drop the current connection temporarily)
    sudo_run(sudo_pw, &["wpa_cli", "enable_network", &net_id]);
    sudo_run(sudo_pw, &["wpa_cli", "select_network", &net_id]);

    // 6. Poll up to 30s
    let mut ok = false;
    for _ in 0..15 {
        thread::sleep(Duration::from_secs(2));
        let status = sudo_output(sudo_pw, &["wpa_cli", "status"]);
        if status.contains("wpa_state=COMPLETED") {
            ok = true;
            break;
        }
    }

    if ok {
        // Success: save credentials and persist config
        sudo_run(sudo_pw, &["wpa_cli", "save_config"]);
        return (true, psk.to_string());
    }

    // Failure: remove the bad profile
    sudo_run(sudo_pw, &["wpa_cli", "remove_network", &net_id]);

    // Restore previous network if there was one
    if let Some(ref prev_id) = prev_net_id {
        sudo_run(sudo_pw, &["wpa_cli", "select_network", prev_id]);
        // Give it a moment to reconnect
        for _ in 0..5 {
            thread::sleep(Duration::from_secs(2));
            let status = sudo_output(sudo_pw, &["wpa_cli", "status"]);
            if status.contains("wpa_state=COMPLETED") {
                break;
            }
        }
    }

    (false, psk.to_string())
}

// ─── WifiApp impl ─────────────────────────────────────────────────────────────

impl WifiApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            lang: detect_lang(),
            screen: Screen::SudoAuth,
            sudo_password: String::new(),
            sudo_error: None,
            scan: Arc::new(Mutex::new(ScanState::default())),
            last_auto_scan: None,
            dialog: None,
        }
    }

    fn validate_sudo(&self) -> bool {
        sudo_run(&self.sudo_password, &["true"])
    }

    fn trigger_scan(&mut self) {
        {
            let mut s = self.scan.lock().unwrap();
            if s.scanning {
                return;
            }
            s.scanning = true;
        }
        let arc = Arc::clone(&self.scan);
        let pw = self.sudo_password.clone();
        thread::spawn(move || {
            sudo_run(&pw, &["wpa_cli", "scan"]);
            thread::sleep(Duration::from_secs(3));
            let networks = fetch_networks(&pw);
            let connected = fetch_connected(&pw);
            let mut s = arc.lock().unwrap();
            s.networks = networks;
            s.connected_ssid = connected;
            s.scanning = false;
        });
    }
}

// ─── eframe::App ─────────────────────────────────────────────────────────────

impl eframe::App for WifiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let l = self.lang;
        ctx.request_repaint_after(Duration::from_millis(400));

        // ── Sudo screen ───────────────────────────────────────────────────────
        if matches!(self.screen, Screen::SudoAuth) {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(80.0);
                ui.vertical_centered(|ui| {
                    ui.heading(egui::RichText::new(l.title).size(22.0));
                    ui.add_space(24.0);
                    ui.label(l.sudo_prompt);
                    ui.add_space(8.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.sudo_password)
                            .password(true)
                            .hint_text(l.sudo_placeholder)
                            .desired_width(260.0),
                    );
                    resp.request_focus();
                    if let Some(ref err) = self.sudo_error {
                        ui.add_space(6.0);
                        ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err.as_str());
                    }
                    ui.add_space(14.0);
                    let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(l.unlock).clicked() || enter_pressed {
                        if self.validate_sudo() {
                            self.screen = Screen::Main;
                            self.sudo_error = None;
                        } else {
                            self.sudo_error = Some(l.wrong_sudo.to_string());
                            self.sudo_password.clear();
                        }
                    }
                });
            });
            return;
        }

        // ── Auto-scan ─────────────────────────────────────────────────────────
        let need_scan = self
            .last_auto_scan
            .map(|t| t.elapsed() > Duration::from_secs(60))
            .unwrap_or(true);
        if need_scan && !self.scan.lock().unwrap().scanning {
            self.last_auto_scan = Some(Instant::now());
            self.trigger_scan();
        }

        // ── Toolbar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new(l.title).size(18.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let scanning = self.scan.lock().unwrap().scanning;
                    if scanning {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(l.scanning).color(egui::Color32::GRAY),
                        );
                    } else if ui.button(l.refresh).clicked() {
                        self.last_auto_scan = Some(Instant::now());
                        self.trigger_scan();
                    }
                });
            });
            ui.add_space(4.0);
        });

        // ── Network list ──────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            let (networks, connected_ssid, scanning) = {
                let s = self.scan.lock().unwrap();
                (s.networks.clone(), s.connected_ssid.clone(), s.scanning)
            };

            if networks.is_empty() {
                ui.centered_and_justified(|ui| {
                    if scanning {
                        ui.spinner();
                    } else {
                        ui.label(
                            egui::RichText::new(l.no_networks)
                                .color(egui::Color32::GRAY),
                        );
                    }
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for net in &networks {
                    let connected = net.ssid == connected_ssid;
                    let (bars, bar_color) = signal_bars(net.signal);

                    let row_resp = ui.horizontal(|ui| {
                        // ✔ connected indicator (fixed width column)
                        if connected {
                            ui.label(
                                egui::RichText::new("✔")
                                    .color(egui::Color32::from_rgb(80, 210, 80))
                                    .size(14.0)
                                    .monospace(),
                            );
                        } else {
                            ui.label(egui::RichText::new(" ").size(14.0).monospace());
                        }

                        // Signal bars — monospace so columns stay stable
                        ui.label(
                            egui::RichText::new(bars)
                                .color(bar_color)
                                .size(11.0)
                                .monospace(),
                        );

                        // SSID button
                        let ssid_text = if connected {
                            egui::RichText::new(&net.ssid)
                                .color(egui::Color32::from_rgb(80, 210, 80))
                                .strong()
                        } else {
                            egui::RichText::new(&net.ssid)
                        };

                        let btn = ui.add(egui::Button::new(ssid_text).frame(false));

                        if net.is_open() {
                            ui.label(
                                egui::RichText::new(l.open_network)
                                    .color(egui::Color32::GRAY)
                                    .size(11.0)
                                    .italics(),
                            );
                        }

                        // dBm right-aligned
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new(format!("{} dBm", net.signal))
                                        .color(egui::Color32::GRAY)
                                        .size(11.0)
                                        .monospace(),
                                );
                            },
                        );

                        btn
                    });

                    if row_resp.inner.clicked() && self.dialog.is_none() {
                        let saved = lookup_saved_pw(&net.ssid);
						self.dialog = Some(ConnectDialog {
							ssid: net.ssid.clone(),
							is_open: net.is_open(),
							// pre-fill password field if we have a saved one
							password: saved.clone().unwrap_or_default(),
							show_password: false,
							connecting: false,
							result: Arc::new(Mutex::new(None)),
							saved_password: saved,
						});
                    }

                    ui.separator();
                }
            });
        });

        // ── Connect dialog ────────────────────────────────────────────────────
        let mut close_dialog = false;

        if let Some(ref mut dlg) = self.dialog {
            // Poll background result
            let maybe_result = dlg.result.lock().unwrap().clone();
            if maybe_result.is_some() {
                dlg.connecting = false;
                // Also refresh connected ssid in the list
                let connected = {
                    let s = self.scan.lock().unwrap();
                    s.connected_ssid.clone()
                };
                let _ = connected; // already updated by thread
            }

            let mut open = true;
            egui::Window::new(format!("{}: {}", l.connect, dlg.ssid))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.set_min_width(320.0);

                    // Show result message
                    let maybe_result = dlg.result.lock().unwrap().clone();
                    if let Some(ref msg) = maybe_result {
                        let color = if msg.contains('✔') {
                            egui::Color32::from_rgb(80, 210, 80)
                        } else {
                            egui::Color32::from_rgb(220, 60, 60)
                        };
                        ui.colored_label(color, msg.as_str());
                        ui.add_space(8.0);
                        if ui.button(l.close).clicked() {
                            close_dialog = true;
                        }
                        return;
                    }

                    // Connecting spinner
                    if dlg.connecting {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                egui::RichText::new(l.connecting)
                                    .color(egui::Color32::GRAY),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                if l as *const _ == &RU as *const _ {
                                    "Подождите, проверяется соединение…"
                                } else {
                                    "Please wait, verifying connection…"
                                }
                            )
                            .color(egui::Color32::GRAY)
                            .size(11.0),
                        );
                        return;
                    }

                    // Password input (for secured networks)
                    if !dlg.is_open {
						// Show hint if using saved password
						if dlg.saved_password.is_some() && dlg.password == dlg.saved_password.clone().unwrap_or_default() {
							ui.label(
								egui::RichText::new(
									if l as *const _ == &RU as *const _ {
										"🔑 Используется сохранённый пароль"
									} else {
										"🔑 Using saved password"
									}
								)
								.color(egui::Color32::from_rgb(100, 180, 255))
								.size(11.0),
							);
						}
						ui.label(l.password_prompt);
						ui.add_space(4.0);
						let pw_resp = ui.add(
							egui::TextEdit::singleline(&mut dlg.password)
								.password(!dlg.show_password)
								.hint_text(l.password_placeholder)
								.desired_width(300.0),
						);
						pw_resp.request_focus();
						ui.checkbox(&mut dlg.show_password, l.show_pw);
						ui.add_space(10.0);
					}

					ui.horizontal(|ui| {
						let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
						// Упростили условие (enter && !dlg.is_open) || (enter && dlg.is_open) до просто enter
						if ui.button(l.connect).clicked() || enter {
							let ssid = dlg.ssid.clone();
							let psk = dlg.password.clone();
							let is_open = dlg.is_open;
							let sudo_pw = self.sudo_password.clone();
							let result_arc = Arc::clone(&dlg.result);
							let scan_arc = Arc::clone(&self.scan);
							let ok_msg = l.connected_ok.to_string();
							let fail_msg = l.connected_fail.to_string();
							dlg.connecting = true;

							thread::spawn(move || {
								// ИСПРАВЛЕНИЕ: Распаковываем кортеж (успех, детали_ошибки)
								let (ok, error_detail) = connect_network(&ssid, &psk, is_open, &sudo_pw);
								
								if ok && !is_open && !psk.is_empty() {
									save_pw_file(&ssid, &psk);
								}

								// Теперь 'ok' имеет тип bool, и компилятор будет доволен
								let (ok, _used_psk) = connect_network(&ssid, &psk, is_open, &sudo_pw);

								if ok && !is_open && !psk.is_empty() {
									save_pw_file(&ssid, &psk);
								}

								let msg = if ok { ok_msg } else { fail_msg };

								// Обновляем статус подключения
								let connected = fetch_connected(&sudo_pw);
								if let Ok(mut s) = scan_arc.lock() {
									s.connected_ssid = connected;
								}

								// Записываем финальное сообщение
								if let Ok(mut res) = result_arc.lock() {
									*res = Some(msg);
								}
							});
						}
						if ui.button(l.cancel).clicked() {
							close_dialog = true;
						}
					});
                });

            if !open {
                close_dialog = true;
            }
        }

        if close_dialog {
            self.dialog = None;
        }
    }
}

// ─── main ─────────────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("odjk Wi-Fi")
            .with_inner_size([440.0, 560.0])
            .with_min_inner_size([340.0, 380.0]),
        ..Default::default()
    };

    eframe::run_native(
        "wifi-manager",
        options,
        // Исправлено: убрали Ok() и возвращаем Box напрямую
        Box::new(|cc| Box::new(WifiApp::new(cc))),
    )
}
