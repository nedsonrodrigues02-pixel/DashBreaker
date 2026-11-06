#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use self_update::cargo_crate_version;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Write;
use std::fs::File;
use std::path::Path;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use eframe::egui::{self, FontData, FontDefinitions, FontId, RichText, Sense, UiBuilder, vec2};
use egui_extras::install_image_loaders;
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use serde::{Deserialize, Serialize};
use steamtools::{Game, Steam, get_games};

mod window;
use window::{InstallPopup, ModsPopup, Plugins, Settings, ViewPopup};

mod utils;
use utils::bserializer::GameMap;
use utils::filter::Filter;

use crate::window::WindowPopup;

#[derive(Deserialize, Serialize, Debug, Default, PartialEq, Clone, Copy)]
enum State {
    #[default]
    Login,
    Setup,
    MainMenu,
    Settings,
}

#[allow(dead_code)]
#[cfg(target_os = "windows")]
const HOOK_DLL: &[u8] = include_bytes!("../deps/xinput1_4.dll");
#[allow(dead_code)]
#[cfg(target_os = "windows")]
const DWMAPI_DLL: &[u8] = include_bytes!("../deps/dwmapi.dll");
#[allow(dead_code)]
#[cfg(target_os = "windows")]
const OPEN_STEAM_TOOL_DLL: &[u8] = include_bytes!("../deps/OpenSteamTool.dll");

const STEAM_BINARY_PATH: &str = "steam.bin";

#[derive(Default)]
struct App {
    st: Steam,
    settings: Settings,
    state: State,
    last_state: State,
    transition_time: f32,
    games: Arc<Mutex<HashMap<u32, Game>>>,
    cached_games: GameMap,
    loaded: bool,
    view: ViewPopup,
    install: InstallPopup,
    mods: ModsPopup,
    plugins: Plugins,
    unlock: bool,
    version: String,
    buffer: String,
    searchbar: RefCell<String>,
    filter: Filter,
    selected_game: Cell<u32>,
    delete_request: Option<u32>,
    #[cfg(not(target_os = "windows"))]
    luatool: bool,
    dark_mode: bool,
    // --- Campos de Login ---
    hwid: String,
    input_key: String,
    login_error: Option<String>,
    is_logging_in: bool,
    // --- Campos de Busca Remota ---
    remote_search_results: Arc<Mutex<Vec<(u32, String)>>>,
    is_searching_remote: Arc<Mutex<bool>>,
    show_search_modal: bool,
    search_query: String,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "dejavu".to_owned(),
            Arc::new(FontData::from_static(include_bytes!(
                "../fonts/DejaVuSans.ttf"
            ))),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "dejavu".to_owned());
        cc.egui_ctx.set_fonts(fonts);

        let hwid = machine_uid::get().unwrap_or_else(|_| "UNKNOWN-HWID".to_string());

        let mut app = App {
            buffer: String::with_capacity(512),
            dark_mode: false,
            transition_time: 1.0,
            hwid,
            ..Default::default()
        };

        if let Some(storage_ref) = cc.storage {
            if let Some(saved_key) = storage_ref.get_string("license_key") {
                app.input_key = saved_key;
                if app.validate_key() {
                    app.state = State::MainMenu;
                }
            }

            if let Some(state) = storage_ref.get_string("state") {
                let parsed_state = serde_json::from_str::<State>(&state).unwrap_or_default();
                if app.state != State::Login {
                    app.state = parsed_state;
                }
                app.last_state = app.state;
            }

            #[cfg(not(target_os = "windows"))]
            if let Some(luatool) = storage_ref.get_string("luatool") {
                app.luatool = serde_json::from_str::<bool>(&luatool).unwrap()
            }

            if let Some(s) = storage_ref.get_string("steam") {
                let mut p = PathBuf::new();
                app.st = serde_json::from_str::<Steam>(&s).unwrap();
                if app.st.cfg.to_string_lossy().is_empty() {
                    p.push(&app.st.path);
                    p.push("config");
                    p.push("stplug-in");
                    app.st.cfg = p;
                }
            }

            if let Some(games) = storage_ref.get_string("games") {
                app.games = serde_json::from_str::<Arc<Mutex<HashMap<u32, Game>>>>(&games).unwrap();
                app.loaded = true;
            }

            if let Some(settings) = storage_ref.get_string("settings") {
                app.settings = serde_json::from_str(&settings).unwrap();
            }

            if let Some(unlock) = storage_ref.get_string("unlock") {
                app.unlock = serde_json::from_str(&unlock).unwrap();
            }
        }

        if app.cached_games.0.is_empty() && Path::new(STEAM_BINARY_PATH).exists() {
            let mut file = File::open(STEAM_BINARY_PATH).unwrap();
            app.cached_games.0 = GameMap::read_from(&mut file).unwrap();
        }

        #[cfg(target_os = "windows")]
        if app.st.path.is_empty() {
            app.st.path = windows_registry::LOCAL_MACHINE
                .open("software\\WOW6432Node\\Valve\\Steam")
                .unwrap()
                .get_string("InstallPath")
                .unwrap();
        }

        app.version = VERSION.to_string();
        app
    }

    fn validate_key(&mut self) -> bool {
        self.login_error = None;
        let client = reqwest::blocking::Client::new();
        
        // ⚠️ SUBSTITUA PELA SUA URL DO CLOUDFLARE AQUI SE AINDA NÃO MUDOU ⚠️
        let api_url = "https://api-licencas-key.cyclonadofps.workers.dev/api/verify"; 

        let body = serde_json::json!({
            "key": self.input_key,
            "hwid": self.hwid
        });

        match client.post(api_url).json(&body).send() {
            Ok(resp) => {
                if resp.status().is_success() {
                    true
                } else {
                    let err_msg = resp.json::<serde_json::Value>()
                        .ok()
                        .and_then(|json| json["error"].as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "Key inválida ou já vinculada a outro PC.".to_string());
                    self.login_error = Some(err_msg);
                    false
                }
            }
            Err(_) => {
                self.login_error = Some("Não foi possível conectar ao servidor de licenças.".to_string());
                false
            }
        }
    }

    // Nova função para pesquisar na Steam via API
    fn perform_steam_search(&self, ctx: egui::Context) {
        let query = self.search_query.trim().to_string();
        if query.is_empty() { return; }

        *self.is_searching_remote.lock().unwrap() = true;
        let results_arc = self.remote_search_results.clone();
        let is_searching_arc = self.is_searching_remote.clone();

        thread::spawn(move || {
            // A API da Steam usa o parâmetro 'term' para buscar
            let url = format!("https://store.steampowered.com/api/storesearch/?term={}&l=portuguese&cc=BR", urlencoding::encode(&query));
            
            let mut found_games = Vec::new();
            if let Ok(resp) = reqwest::blocking::get(url) {
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    if let Some(items) = json["items"].as_array() {
                        for item in items {
                            if let (Some(id), Some(name)) = (item["id"].as_u64(), item["name"].as_str()) {
                                found_games.push((id as u32, name.to_string()));
                            }
                        }
                    }
                }
            }
            
            *results_arc.lock().unwrap() = found_games;
            *is_searching_arc.lock().unwrap() = false;
            ctx.request_repaint();
        });
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("license_key", self.input_key.clone());
        storage.set_string("steam", serde_json::to_string(&self.st).unwrap());
        storage.set_string("version", serde_json::to_string(VERSION).unwrap());
        if self.state == State::Setup {
            storage.set_string("state", serde_json::to_string(&self.state).unwrap());
        } else if self.state != State::Login {
            storage.set_string("state", serde_json::to_string(&State::MainMenu).unwrap());
        }
        storage.set_string("games", serde_json::to_string(&self.games).unwrap());
        storage.set_string("settings", serde_json::to_string(&self.settings).unwrap());
        storage.set_string("unlock", serde_json::to_string(&self.unlock).unwrap());
        #[cfg(not(target_os = "windows"))]
        storage.set_string("luatool", serde_json::to_string(&self.luatool).unwrap())
    }

    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let is_light = !self.dark_mode;
        let mut visuals = if self.dark_mode { egui::Visuals::dark() } else { egui::Visuals::light() };
        
        let panel_bg = if is_light { egui::Color32::from_hex("#F2F2F2").unwrap() } else { egui::Color32::from_gray(30) };
        let text_color = if is_light { egui::Color32::BLACK } else { egui::Color32::WHITE };
        let subtitle_color = if is_light { egui::Color32::from_gray(100) } else { egui::Color32::from_gray(170) };
        let input_bg = if is_light { egui::Color32::from_gray(225) } else { egui::Color32::from_gray(60) };
        let card_bg = if is_light { egui::Color32::WHITE } else { egui::Color32::from_gray(45) };

        if is_light {
            visuals.panel_fill = panel_bg;
            visuals.widgets.inactive.bg_fill = input_bg;
            visuals.widgets.inactive.weak_bg_fill = input_bg;
            visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0f32, egui::Color32::from_gray(180));
            visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0f32, egui::Color32::from_gray(200));
        }
        ui.ctx().set_visuals(visuals);

        if self.state != self.last_state {
            self.transition_time = 0.0;
            self.last_state = self.state;
        }
        self.transition_time = (self.transition_time + ui.input(|i| i.stable_dt) * 6.0).min(1.0);
        let alpha = self.transition_time.clamp(0.0, 1.0);
        ui.set_opacity(alpha);

        egui::Panel::top("custom_titlebar")
            .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(egui::Margin::symmetric(10, 8)))
            .show_inside(ui, |ui| {
                let response = ui.interact(ui.max_rect(), egui::Id::new("title_bar_drag"), egui::Sense::drag());
                if response.dragged() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                
                ui.horizontal(|ui| {
                    let logo = egui::Image::from_bytes("bytes://logo.png", include_bytes!("../icon.png").to_vec())
                        .fit_to_exact_size(egui::vec2(22.0, 22.0))
                        .corner_radius(5.0);
                    ui.add(logo);
                    ui.add_space(8.0);
                    ui.label(RichText::new("DashBreaker").strong().size(16.0).color(text_color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new("✕").color(text_color)).clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });

        match self.state {
            // ... (Login e Setup mantidos iguais, omitidos aqui para focar na mudança)
            State::Login => { /* Código de Login mantido igual */ }
            State::Setup => { /* Código de Setup mantido igual */ }
            State::Settings => { /* Código de Settings mantido igual */ }

            State::MainMenu => {
                ViewPopup::view(self, ui);
                Plugins::view(self, ui);
                Plugins::ceditor(self, ui);
                ModsPopup::view(self, ui);
                InstallPopup::view(self, ui);

                // JANELA DE RESULTADOS DA BUSCA REMOTA
                if self.show_search_modal {
                    egui::Window::new("Resultados da Busca (Steam)")
                        .collapsible(false)
                        .resizable(true)
                        .default_size([400.0, 300.0])
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ui.ctx(), |ui| {
                            if *self.is_searching_remote.lock().unwrap() {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(20.0);
                                    ui.add(egui::Spinner::new().size(30.0));
                                    ui.add_space(10.0);
                                    ui.label("Buscando na Steam...");
                                });
                            } else {
                                let results = self.remote_search_results.lock().unwrap().clone();
                                if results.is_empty() {
                                    ui.label("Nenhum jogo encontrado.");
                                } else {
                                    egui::ScrollArea::vertical().show(ui, |ui| {
                                        for (appid, name) in results {
                                            if ui.button(format!("{} (AppID: {})", name, appid)).clicked() {
                                                // Preenche o AppID no popup de download e fecha a busca
                                                self.install.appid = appid.to_string();
                                                self.install.active = true;
                                                self.show_search_modal = false;
                                            }
                                        }
                                    });
                                }
                            }
                            ui.add_space(10.0);
                            if ui.button("Fechar").clicked() {
                                self.show_search_modal = false;
                            }
                        });
                }

                egui::Panel::top("top").show_inside(ui, |ui| {
                    ui.add_space(10.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Início").font(FontId::proportional(28.0)).color(text_color).strong());
                        ui.label(RichText::new("Visão geral dos seus jogos").size(15.0).color(subtitle_color));
                    });
                    ui.add_space(15.0);
                    
                    ui.horizontal(|ui| {
                        let total_width = ui.available_width();
                        let search_width = 250.0;
                        let spacing = 10.0;
                        let btn_width = 28.0;
                        let btn_steam_width = 130.0; // Largura do botão da Steam
                        let install_btn_width = 110.0;
                        
                        let total_elements = search_width + spacing + btn_width + spacing + btn_steam_width + spacing + install_btn_width;
                        let margin = (total_width - total_elements) / 2.0;
                        if margin > 0.0 { ui.add_space(margin); }

                        let searchbar = ui.add_sized([search_width, 28.0], egui::TextEdit::singleline(self.searchbar.get_mut())
                            .hint_text("Buscar na sua lista")
                            .background_color(input_bg)
                            .text_color(text_color)
                        );
                        
                        let btn_clear = egui::Button::new(RichText::new("✕").color(text_color)).fill(input_bg);
                        if ui.add_sized([btn_width, 28.0], btn_clear).clicked() {
                            self.searchbar.get_mut().clear();
                            self.filter = Filter::None;
                        }

                        ui.add_space(5.0);

                        // BOTÃO NOVO: BUSCAR NA STEAM
                        let btn_search_steam = egui::Button::new(RichText::new("🔍 Buscar na Steam").color(text_color).size(13.0)).fill(input_bg);
                        if ui.add_sized([btn_steam_width, 28.0], btn_search_steam).clicked() {
                            self.search_query = self.searchbar.borrow().clone();
                            if !self.search_query.is_empty() {
                                self.show_search_modal = true;
                                self.perform_steam_search(ui.ctx().clone());
                            }
                        }

                        ui.add_space(5.0);

                        let btn_install_lua = egui::Button::new(RichText::new("+ Baixar .lua").color(text_color).size(13.0)).fill(input_bg);
                        if ui.add_sized([install_btn_width, 28.0], btn_install_lua).clicked() {
                            self.install.active = true;
                        }
                        
                        if searchbar.changed() {
                            if self.searchbar.borrow().is_empty() {
                                self.filter = Filter::None;
                            } else {
                                match &self.searchbar.borrow().parse::<u32>() {
                                    Ok(id) => self.filter = Filter::Id(*id),
                                    Err(_) => self.filter = Filter::Name(self.searchbar.borrow().clone()),
                                }
                            }
                        }
                    });
                    ui.add_space(10.0);
                });

                // ... (Restante do código da UI mantido idêntico)
                egui::Panel::left("nav_panel").exact_size(60.0).resizable(false).show_inside(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(15.0);
                            if ui.button(RichText::new(if self.dark_mode { "☀" } else { "🌙" }).size(20.0).color(text_color)).clicked() {
                                self.dark_mode = !self.dark_mode;
                            }
                            ui.add_space(15.0);
                            if ui.button(RichText::new("⚙").size(20.0).color(text_color)).clicked() {
                                self.state = State::Settings;
                            }
                            ui.add_space(15.0);
                            if ui.button(RichText::new("↻").size(20.0).color(text_color)).clicked() {
                                self.plugins.fetched = false;
                                self.loaded = false;
                            }
                            ui.add_space(15.0);
                            if ui.button(RichText::new("↺").size(20.0).color(text_color)).clicked() {
                                let files = rfd::FileDialog::default().add_filter("lua | manifest", &["lua", "manifest"]).pick_files();
                                if let Some(ref files) = files {
                                    let mut path = PathBuf::from(&self.st.path);
                                    if !path.exists() { fs::create_dir(&path).ok(); }
                                    for file in files {
                                        if file.extension().is_some_and(|x| x == "manifest") {
                                            path.push("depotcache");
                                            path.push(file.file_stem().unwrap());
                                            fs::copy(file.as_path(), format!("{}.manifest", path.to_string_lossy())).unwrap();
                                            path.pop(); path.pop();
                                            continue;
                                        }
                                        #[cfg(target_os = "windows")] path.push("config\\stplug-in");
                                        path.push(file.file_stem().unwrap());
                                        fs::copy(file.as_path(), format!("{}.lua", path.to_string_lossy())).unwrap();
                                        path.pop(); path.pop();
                                    }
                                }
                                self.loaded = false;
                            }
                        });
                    });

                egui::Panel::right("game_stats").exact_size(160.0).show_separator_line(true).resizable(false).show_inside(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            let width = ui.available_width();
                            let height = ui.available_height();
                            let selected_game = self.selected_game.get();
                            
                            if selected_game != 0 {
                                let game_opt = { self.games.lock().unwrap().get(&selected_game).cloned() };
                                if let Some(game) = game_opt {
                                    ui.add_space(10.0);
                                    ui.label(RichText::new(&game.details.name).font(FontId::new(18.0, egui::FontFamily::Proportional)).color(text_color).strong());
                                    ui.add_space(5.0);
                                    ui.label(RichText::new(format!("APPID: {}", game.appid)).color(text_color));
                                    ui.add_space(10.0);
                                    
                                    ui.vertical_centered_justified(|ui| {
                                        let btn_size = vec2((width * 0.1).clamp(50.0, 90.0), (height * 0.1).clamp(25.0, 45.0));
                                        if game.installed {
                                            if ui.add_sized(btn_size, egui::Button::new(RichText::new("🗑 Desinstalar").strong().color(text_color)).fill(input_bg)).clicked() {
                                                self.buffer.clear();
                                                write!(&mut self.buffer, "start steam://uninstall/{}", game.appid).unwrap();
                                                #[cfg(target_os = "windows")]
                                                std::process::Command::new("cmd").args(["/C", &self.buffer]).spawn().unwrap().wait().unwrap();
                                                self.buffer.clear();
                                            }
                                        } else if ui.add_sized(btn_size, egui::Button::new(RichText::new("+ Instalar").strong().color(text_color)).fill(input_bg)).clicked() {
                                            self.buffer.clear();
                                            write!(&mut self.buffer, "start steam://install/{}", game.appid).unwrap();
                                            #[cfg(target_os = "windows")]
                                            std::process::Command::new("cmd").args(["/C", &self.buffer]).spawn().unwrap().wait().unwrap();
                                            self.buffer.clear();
                                        }

                                        ui.add_space(5.0);
                                        if ui.add_sized(btn_size, egui::Button::new(RichText::new("🔍 Ver").strong().color(text_color)).fill(input_bg)).clicked() {
                                            self.view.current_game = game.appid;
                                            self.view.active = true;
                                        }
                                        ui.add_space(5.0);
                                        if ui.add_sized(btn_size, egui::Button::new(RichText::new("Remover").strong().color(text_color)).fill(input_bg)).clicked() {
                                            let mut p = PathBuf::from(&self.st.path);
                                            p.push("config"); p.push("stplug-in");
                                            self.buffer.clear(); write!(&mut self.buffer, "{}.lua", game.appid).unwrap();
                                            p.push(&self.buffer); self.buffer.clear();
                                            if fs::remove_file(&p).is_err() {
                                                rfd::MessageDialog::new().set_title("Erro").set_description("Falha ao deletar").set_buttons(rfd::MessageButtons::Ok);
                                            }
                                            p.clear(); p.push("icons"); p.push(format!("{}.jpg", game.appid));
                                            fs::remove_file(&p).ok();
                                            self.delete_request = Some(game.appid);
                                        }
                                    });
                                }
                            } else {
                                ui.add_space(20.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.wrap_mode();
                                    ui.label(RichText::new("Selecione um jogo clicando nele!").color(text_color));
                                });
                            }
                        });
                    });

                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                        if !self.loaded {
                            ui.vertical_centered(|ui| {
                                ui.add_space(100.0);
                                ui.add(egui::Spinner::new().size(50.0).color(if is_light { egui::Color32::DARK_GRAY } else { egui::Color32::WHITE }));
                                ui.add_space(15.0);
                                ui.label(RichText::new("Carregando seus jogos...").color(subtitle_color).size(16.0));
                            });
                        } else {
                            ui.add_space(15.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(15.0, 15.0);
                                let game_map = { self.games.lock().unwrap().clone() };

                                if let Some(s) = self.delete_request.take() {
                                    self.selected_game.set(0);
                                    self.games.lock().unwrap().remove(&s);
                                    self.loaded = false;
                                }

                                for (id, _) in game_map.iter().filter(|(gid, g)| match &self.filter {
                                    Filter::Id(id) => id == *gid,
                                    Filter::Name(name) => g.details.name.to_lowercase().starts_with(&*name.to_lowercase()),
                                    Filter::None => true,
                                }) {
                                    let width = 240.0;
                                    let height = 112.0;
                                    let (card_rect, card_resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());

                                    self.buffer.clear();
                                    write!(&mut self.buffer, "icons/{id}.jpg").unwrap();
                                    let img_path = Path::new(&self.buffer);

                                    ui.scope_builder(UiBuilder::new().max_rect(card_rect), |ui| {
                                        if img_path.exists() {
                                            ui.add(
                                                egui::Image::new(format!("file://{}", self.buffer))
                                                    .corner_radius(egui::CornerRadius::same(8))
                                                    .fit_to_exact_size(egui::vec2(width, height)),
                                            );
                                        } else {
                                            let rect = egui::Rect::from_min_size(ui.min_rect().min, egui::vec2(width, height));
                                            ui.painter().rect_filled(rect, 8.0, egui::Color32::from_gray(180));
                                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Carregando...", FontId::proportional(14.0), egui::Color32::WHITE);
                                        }
                                    });

                                    if card_resp.hovered() {
                                        ui.painter().rect_stroke(
                                            card_resp.rect.expand(2.0), 10.0,
                                            egui::Stroke::new(2.0f32, ui.visuals().selection.stroke.color),
                                            egui::StrokeKind::Middle,
                                        );
                                    }

                                    if card_resp.interact(Sense::click()).clicked() {
                                        self.selected_game.set(*id);
                                    }
                                }
                            });
                        }
                    });
                });
                
                if !self.loaded {
                    ui.request_repaint();
                    let s = self.st.path.clone();
                    let games_arc = self.games.clone();
                    thread::spawn(move || {
                        let mut sbin = fs::File::create(STEAM_BINARY_PATH).unwrap();
                        let current_games = { games_arc.lock().unwrap().clone() };
                        GameMap::write_to(&mut sbin, &current_games).unwrap();
                        let Ok(result) = get_games(&s, current_games) else { return; };
                        let mut games = games_arc.lock().unwrap();
                        *games = result;
                    });
                    self.loaded = true;
                }
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    // ... (Configurações de log mantidas)
    let mut viewport = egui::ViewportBuilder::default()
        .with_taskbar(true)
        .with_inner_size([1050.0, 650.0])
        .with_min_inner_size([850.0, 550.0])
        .with_decorations(false)
        .with_title("DashBreaker");

    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../icon.png")) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        centered: true,
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "DashBreaker",
        options,
        Box::new(|cc| {
            install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(App::new(cc)))
        }),
    )
}