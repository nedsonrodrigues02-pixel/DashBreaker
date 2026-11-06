use crate::window::WindowPopup;
use eframe::egui::{self, Window};
use log::debug;
use std::{
    fs::File,
    io::{self, Error, ErrorKind, Write as ioWrite},
};

struct Handler(fn(u32) -> io::Result<Vec<u8>>);

pub struct InstallPopup {
    pub active: bool,
    pub appid: String,
    handler: Handler,
}

/// Baixa o .lua diretamente do repositório oficial do GitHub (sem bloqueios)
fn download_lua(appid: u32) -> io::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .build()
        .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))?;

    // A URL exata que o site usa nos bastidores (conforme o llms.txt)
    let raw_github_url = format!(
        "https://raw.githubusercontent.com/steamtoolsapp/ManifestHub/{0}/{0}.lua",
        appid
    );

    debug!("Tentando baixar direto do GitHub: {}", raw_github_url);

    let resp = client.get(&raw_github_url).send().map_err(|_| {
        Error::new(
            ErrorKind::ConnectionAborted,
            "Falha ao conectar com o GitHub. Verifique sua internet.",
        )
    })?;

    if !resp.status().is_success() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "Arquivo .lua não encontrado para este AppID no repositório.",
        ));
    }

    let bytes = resp.bytes().map_err(|_| {
        Error::new(ErrorKind::InvalidData, "Falha ao ler o arquivo do GitHub.")
    })?;

    // Verifica se o conteúdo é realmente um script Lua e não uma página de erro (HTML)
    if bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<html") {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "O GitHub retornou uma página HTML em vez do arquivo Lua.",
        ));
    }

    Ok(bytes.to_vec())
}

impl Default for InstallPopup {
    fn default() -> Self {
        Self {
            active: false,
            appid: String::new(),
            handler: Handler(download_lua),
        }
    }
}

impl WindowPopup for InstallPopup {
    fn view(app: &mut crate::App, ui: &mut egui::Ui) {
        Window::new("Baixar .lua")
            .default_size([0.0, 0.0])
            .open(&mut app.install.active)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("AppID: ");
                        let response = ui.text_edit_singleline(&mut app.install.appid);
                        if response.changed()
                            && !app.install.appid.chars().all(|c| c.is_ascii_digit())
                        {
                            app.install.appid.retain(|c| c.is_ascii_digit());
                        }
                    });

                    if ui.button("Baixar").clicked() && !app.install.appid.is_empty() {
                        if let Ok(parsed_id) = app.install.appid.parse::<u32>() {
                            match (app.install.handler.0)(parsed_id) {
                                Ok(lua_bytes) => {
                                    // Abre a janela "Salvar Como" para o usuário escolher onde salvar.
                                    let save_path = rfd::FileDialog::new()
                                        .set_title("Salvar arquivo .lua")
                                        .set_file_name(format!("{}.lua", parsed_id))
                                        .add_filter("Lua Script", &["lua"])
                                        .save_file();

                                    if let Some(path) = save_path {
                                        if let Ok(mut file) = File::create(path) {
                                            if file.write_all(&lua_bytes).is_ok() {
                                                rfd::MessageDialog::new()
                                                    .set_title("Sucesso")
                                                    .set_description("Arquivo .lua baixado e salvo com sucesso!\n\nAgora use o botão 'Importar' (↺) no app para adicioná-lo à sua Steam.")
                                                    .show();
                                            } else {
                                                rfd::MessageDialog::new()
                                                    .set_title("Erro")
                                                    .set_description("Não foi possível salvar o arquivo .lua no local escolhido.")
                                                    .set_level(rfd::MessageLevel::Error)
                                                    .show();
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    rfd::MessageDialog::new()
                                        .set_title("Erro no Download")
                                        .set_level(rfd::MessageLevel::Error)
                                        .set_description(e.to_string())
                                        .set_buttons(rfd::MessageButtons::Ok)
                                        .show();
                                }
                            }
                        }
                    }
                });
            });
    }
}