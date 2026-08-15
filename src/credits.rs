use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
const PUBLIC_LOOKUP_URL: &str = "https://discordlookup.org/api/discord-lookup?id=";
const CREDIT_SUGGESTION_CONTACT_ID: &str = "751250088910913647";
const ART_FRAME_STROKE_WIDTH: f32 = 1.5;
const ART_FRAME_SIDE_GUTTER: i8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CreditEntry {
    id: &'static str,
    contribution: Option<&'static str>,
}

impl CreditEntry {
    const fn person(id: &'static str) -> Self {
        Self {
            id,
            contribution: None,
        }
    }

    const fn upstream(id: &'static str, contribution: &'static str) -> Self {
        Self {
            id,
            contribution: Some(contribution),
        }
    }
}

const SUPPORT: &[CreditEntry] = &[
    CreditEntry::person("770942826519199784"),
    CreditEntry::person("318117607003652109"),
    CreditEntry::person("195577361033330688"),
    CreditEntry::person("340711847420231691"),
    CreditEntry::person("188362544283516928"),
    CreditEntry::person("1377069620434960404"),
    CreditEntry::person("120364653795606529"),
    CreditEntry::person("744631056522674246"),
];

const TESTING: &[CreditEntry] = &[
    CreditEntry::person("195577361033330688"),
    CreditEntry::person("188362544283516928"),
    CreditEntry::person("1377069620434960404"),
];

const ART: &[CreditEntry] = &[CreditEntry::person("385629478308806666")];

const UPSTREAM_DEVELOPMENT: &[CreditEntry] = &[
    CreditEntry::upstream("125287732439285761", "Switch Toolbox and BNTX tooling"),
    CreditEntry::upstream("179233946012352512", "ARCropolis tools and Smash research"),
    CreditEntry::upstream(
        "340711847420231691",
        "EffectResearch and effect documentation",
    ),
    CreditEntry::upstream(
        "214199485105045504",
        "SSBH rendering, file formats, textures, and ArcExplorer",
    ),
    CreditEntry::upstream(
        "120364653795606529",
        "Parameter labels and dumped-script contributions",
    ),
];

const SECTIONS: &[(&str, &[CreditEntry])] = &[
    ("Support", SUPPORT),
    ("Testing", TESTING),
    ("Art", ART),
    ("Upstream Development", UPSTREAM_DEVELOPMENT),
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscordProfile {
    id: String,
    username: String,
    display_name: String,
    avatar_url: String,
}

impl DiscordProfile {
    fn fallback(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            username: "Discord profile".to_owned(),
            display_name: "Discord profile".to_owned(),
            avatar_url: default_avatar_url(id),
        }
    }

    fn from_parts(
        id: String,
        username: String,
        global_name: Option<String>,
        avatar: Option<String>,
    ) -> Self {
        let display_name = global_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| username.clone());
        let avatar_url = avatar_url(&id, avatar.as_deref());
        Self {
            id,
            username,
            display_name,
            avatar_url,
        }
    }
}

#[derive(Deserialize)]
struct PublicLookupResponse {
    user: PublicLookupUser,
}

#[derive(Deserialize)]
struct PublicLookupUser {
    id: String,
    username: String,
    #[serde(rename = "globalName")]
    global_name: Option<String>,
    avatar: Option<String>,
}

#[derive(Deserialize)]
struct DiscordUserResponse {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

type FetchResult = (String, Result<DiscordProfile, String>);

/// Native Credits viewport with asynchronously refreshed Discord identity cards.
pub struct CreditsWindow {
    pub open: bool,
    profiles: HashMap<String, DiscordProfile>,
    errors: HashMap<String, String>,
    receiver: Option<Receiver<Vec<FetchResult>>>,
    last_refresh: Option<Instant>,
}

impl Default for CreditsWindow {
    fn default() -> Self {
        let profiles = unique_ids()
            .into_iter()
            .map(|id| (id.to_owned(), DiscordProfile::fallback(id)))
            .collect();
        Self {
            open: false,
            profiles,
            errors: HashMap::new(),
            receiver: None,
            last_refresh: None,
        }
    }
}

impl CreditsWindow {
    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll_fetch(ctx);
        if !self.open {
            return;
        }

        if self.receiver.is_none()
            && self
                .last_refresh
                .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL)
        {
            self.start_fetch(ctx);
        }

        let mut refresh = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("credits"),
            egui::ViewportBuilder::default()
                .with_app_id(crate::app_icon::APP_ID)
                .with_icon(crate::app_icon::viewport_icon())
                .with_title("Credits — Visionary")
                .with_inner_size([900.0, 680.0])
                .with_min_inner_size([440.0, 340.0]),
            |ui, class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(egui::Color32::from_rgb(13, 14, 24)))
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let page_width = ui.available_width();
                            let title_size = responsive_size(page_width, 26.0, 32.0);
                            ui.add_space(responsive_size(page_width, 8.0, 14.0));
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("CREDITS")
                                        .size(title_size)
                                        .strong()
                                        .color(egui::Color32::from_rgb(235, 238, 255)),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "The people who helped make Visionary possible",
                                    )
                                    .size(responsive_size(page_width, 12.0, 14.0))
                                    .color(egui::Color32::from_rgb(157, 164, 198)),
                                );
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if self.receiver.is_some() {
                                        ui.spinner();
                                        ui.label(
                                            egui::RichText::new("Refreshing profiles…")
                                                .small()
                                                .color(egui::Color32::LIGHT_GRAY),
                                        );
                                    } else {
                                        let status = if self.errors.is_empty() {
                                            "Discord profiles are up to date".to_owned()
                                        } else {
                                            format!(
                                                "{} profile{} could not be refreshed",
                                                self.errors.len(),
                                                if self.errors.len() == 1 { "" } else { "s" }
                                            )
                                        };
                                        ui.label(
                                            egui::RichText::new(status)
                                                .small()
                                                .color(egui::Color32::GRAY),
                                        );
                                    }
                                    if ui
                                        .add_enabled(
                                            self.receiver.is_none(),
                                            egui::Button::new("Refresh"),
                                        )
                                        .on_hover_text(
                                            "Fetch current names and avatars. Profiles also refresh automatically.",
                                        )
                                        .clicked()
                                    {
                                        refresh = true;
                                    }
                                });
                            });

                            ui.add_space(responsive_size(page_width, 8.0, 12.0));
                            for (index, (title, ids)) in SECTIONS.iter().enumerate() {
                                if index > 0 {
                                    ui.add_space(responsive_size(page_width, 12.0, 18.0));
                                }
                                if *title == "Art" {
                                    self.draw_art_section(ui, ids);
                                } else {
                                    self.draw_section(ui, title, ids);
                                }
                            }
                            ui.add_space(responsive_size(page_width, 18.0, 24.0));
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "Think someone should be added to the credits?",
                                    )
                                    .size(responsive_size(page_width, 12.0, 14.0))
                                    .color(egui::Color32::from_rgb(185, 191, 224)),
                                );
                                ui.hyperlink_to(
                                    egui::RichText::new("Message me on Discord")
                                    .size(responsive_size(page_width, 12.0, 14.0))
                                    .strong()
                                    .color(egui::Color32::from_rgb(181, 190, 255)),
                                    credit_suggestion_contact_url(),
                                );
                            });
                            ui.add_space(16.0);
                        });
                    });

                if class != egui::ViewportClass::EmbeddedWindow
                    && ui.ctx().input(|input| input.viewport().close_requested())
                {
                    self.open = false;
                }
            },
        );

        if refresh {
            self.start_fetch(ctx);
        } else if let Some(last) = self.last_refresh {
            ctx.request_repaint_after(REFRESH_INTERVAL.saturating_sub(last.elapsed()));
        }
    }

    fn draw_section(&self, ui: &mut egui::Ui, title: &str, entries: &[CreditEntry]) {
        let available_width = ui.available_width();
        ui.label(
            egui::RichText::new(title.to_uppercase())
                .size(responsive_size(available_width, 16.0, 19.0))
                .strong()
                .color(egui::Color32::from_rgb(185, 191, 224)),
        );
        ui.add_space(6.0);

        let metrics = grid_metrics(available_width, entries.len());
        for (row_index, row) in entries.chunks(metrics.columns).enumerate() {
            let row_width = metrics.card_width * row.len() as f32
                + metrics.gap * row.len().saturating_sub(1) as f32;
            let side_space = ((available_width - row_width) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = metrics.gap;
                ui.add_space(side_space);
                for entry in row {
                    let profile = self.profile(entry.id);
                    profile_card(
                        ui,
                        profile,
                        metrics.card_width,
                        metrics.avatar_size,
                        entry.contribution,
                        false,
                    );
                }
            });
            if row_index + 1 < entries.len().div_ceil(metrics.columns) {
                ui.add_space(metrics.gap);
            }
        }
    }

    fn draw_art_section(&self, ui: &mut egui::Ui, entries: &[CreditEntry]) -> egui::Response {
        let accent = egui::Color32::from_rgb(238, 158, 255);
        let available_width = ui.available_width();
        let frame = art_frame(available_width, accent);
        let content_width = (available_width - frame.total_margin().sum().x).max(1.0);
        frame
            .show(ui, |ui| {
                ui.set_width(content_width);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("✦  ART  ✦")
                            .size(responsive_size(available_width, 23.0, 28.0))
                            .strong()
                            .color(accent),
                    );
                    ui.add_space(8.0);
                    let (card_width, avatar_size) = art_profile_sizes(content_width);
                    for entry in entries {
                        let profile = self.profile(entry.id);
                        profile_card(ui, profile, card_width, avatar_size, None, true);
                    }
                });
            })
            .response
    }

    fn profile(&self, id: &str) -> &DiscordProfile {
        self.profiles
            .get(id)
            .expect("every credits ID has a fallback profile")
    }

    fn start_fetch(&mut self, ctx: &egui::Context) {
        if self.receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let ids: Vec<String> = unique_ids().into_iter().map(str::to_owned).collect();
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(4))
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("Visionary/", env!("CARGO_PKG_VERSION")))
                .build();
            let results = match client {
                Ok(client) => ids
                    .into_iter()
                    .map(|id| {
                        let result = fetch_profile(&client, &id);
                        (id, result)
                    })
                    .collect(),
                Err(error) => ids
                    .into_iter()
                    .map(|id| (id, Err(format!("could not create HTTP client: {error}"))))
                    .collect(),
            };
            let _ = tx.send(results);
            ctx.request_repaint();
        });
        self.receiver = Some(rx);
    }

    fn poll_fetch(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.receiver else {
            return;
        };
        let Ok(results) = receiver.try_recv() else {
            return;
        };
        self.errors.clear();
        for (id, result) in results {
            match result {
                Ok(profile) => {
                    self.profiles.insert(id, profile);
                }
                Err(error) => {
                    self.errors.insert(id, error);
                }
            }
        }
        self.receiver = None;
        self.last_refresh = Some(Instant::now());
        ctx.request_repaint();
    }
}

fn profile_card(
    ui: &mut egui::Ui,
    profile: &DiscordProfile,
    card_width: f32,
    avatar_size: f32,
    contribution: Option<&str>,
    featured: bool,
) {
    let margin = if featured { 15 } else { 8 };
    let fill = if featured {
        egui::Color32::from_rgb(52, 29, 70)
    } else {
        egui::Color32::from_rgb(25, 27, 43)
    };
    let stroke = if featured {
        egui::Stroke::new(1.5, egui::Color32::from_rgb(215, 121, 237))
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(54, 59, 84))
    };
    let frame = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(if featured { 14.0 } else { 10.0 })
        .inner_margin(egui::Margin::symmetric(margin, margin));
    let content_width = (card_width - frame.total_margin().sum().x).max(1.0);
    frame.show(ui, |ui| {
        ui.set_width(content_width);
        ui.vertical_centered(|ui| {
            ui.add(
                egui::Image::new(profile.avatar_url.as_str())
                    .fit_to_exact_size(egui::vec2(avatar_size, avatar_size))
                    .corner_radius(avatar_size / 2.0),
            )
            .on_hover_text(format!("Discord user {}", profile.id));
            ui.add_space(if featured { 8.0 } else { 5.0 });
            ui.hyperlink_to(
                egui::RichText::new(&profile.display_name)
                    .size(if featured {
                        (card_width * 0.09).clamp(18.0, 21.0)
                    } else {
                        (card_width * 0.10).clamp(12.0, 14.0)
                    })
                    .strong()
                    .color(if featured {
                        egui::Color32::from_rgb(251, 221, 255)
                    } else {
                        egui::Color32::from_rgb(230, 233, 249)
                    }),
                format!("https://discord.com/users/{}", profile.id),
            );
            if profile.username != profile.display_name {
                ui.label(
                    egui::RichText::new(format!("@{}", profile.username))
                        .size(if featured { 12.0 } else { 10.0 })
                        .color(egui::Color32::from_rgb(148, 154, 184)),
                );
            }
            if let Some(contribution) = contribution {
                ui.add_space(3.0);
                add_contribution_note(ui, contribution);
            }
        });
    });
}

fn add_contribution_note(ui: &mut egui::Ui, contribution: &str) -> egui::Response {
    ui.add(
        egui::Label::new(
            egui::RichText::new(contribution)
                .size(9.0)
                .color(egui::Color32::from_rgb(157, 164, 198)),
        )
        .wrap(),
    )
}

fn art_frame(available_width: f32, accent: egui::Color32) -> egui::Frame {
    let frame_margin = responsive_size(available_width, 10.0, 16.0).round() as i8;
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(35, 19, 52))
        .stroke(egui::Stroke::new(ART_FRAME_STROKE_WIDTH, accent))
        .corner_radius(14.0)
        .inner_margin(egui::Margin::symmetric(frame_margin, frame_margin))
        .outer_margin(egui::Margin::symmetric(ART_FRAME_SIDE_GUTTER, 0))
}

fn art_profile_sizes(content_width: f32) -> (f32, f32) {
    let card_width = (content_width * 0.28)
        .clamp(180.0, 238.0)
        .min(content_width);
    let avatar_size = (content_width * 0.14)
        .clamp(92.0, 128.0)
        .min((card_width - 30.0).max(1.0));
    (card_width, avatar_size)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GridMetrics {
    columns: usize,
    card_width: f32,
    avatar_size: f32,
    gap: f32,
}

fn grid_metrics(available_width: f32, item_count: usize) -> GridMetrics {
    let gap = 8.0;
    let available_width = available_width.max(1.0);
    let mut columns = (((available_width + gap) / 128.0).floor() as usize)
        .max(1)
        .min(item_count.max(1));

    // Avoid a nearly empty final row as the window crosses a column breakpoint.
    while columns > 2 {
        let remainder = item_count % columns;
        if remainder == 0 || remainder * 2 >= columns {
            break;
        }
        columns -= 1;
    }

    let card_width =
        ((available_width - gap * columns.saturating_sub(1) as f32) / columns as f32).max(1.0);
    GridMetrics {
        columns,
        card_width,
        avatar_size: (card_width * 0.47).clamp(50.0, 72.0),
        gap,
    }
}

fn responsive_size(available_width: f32, compact: f32, roomy: f32) -> f32 {
    let t = ((available_width - 440.0) / 600.0).clamp(0.0, 1.0);
    compact + (roomy - compact) * t
}

fn credit_suggestion_contact_url() -> String {
    format!("https://discord.com/users/{CREDIT_SUGGESTION_CONTACT_ID}")
}

fn fetch_profile(client: &reqwest::blocking::Client, id: &str) -> Result<DiscordProfile, String> {
    let official_error = match std::env::var("VISIONARY_DISCORD_BOT_TOKEN") {
        Ok(token) if !token.trim().is_empty() => match fetch_official(client, id, token.trim()) {
            Ok(profile) => return Ok(profile),
            Err(error) => Some(error),
        },
        _ => None,
    };

    let public = fetch_public(client, id);

    match (public, official_error) {
        (Ok(profile), _) => Ok(profile),
        (Err(public_error), Some(official_error)) => {
            Err(format!("{official_error}; {public_error}"))
        }
        (Err(error), None) => Err(error),
    }
}

fn fetch_public(client: &reqwest::blocking::Client, id: &str) -> Result<DiscordProfile, String> {
    for attempt in 0..=2 {
        let response = client
            .get(format!("{PUBLIC_LOOKUP_URL}{id}"))
            .send()
            .map_err(|error| format!("public profile lookup failed: {error}"))?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
            let wait = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<f32>().ok())
                .map(|seconds| Duration::from_secs_f32(seconds.clamp(0.25, 3.0)))
                .unwrap_or(Duration::from_secs(1));
            std::thread::sleep(wait);
            continue;
        }

        let user = response
            .error_for_status()
            .and_then(reqwest::blocking::Response::json::<PublicLookupResponse>)
            .map_err(|error| format!("public profile lookup failed: {error}"))?
            .user;
        if user.id != id {
            return Err(format!(
                "public profile lookup returned user {} for requested user {id}",
                user.id
            ));
        }
        return Ok(DiscordProfile::from_parts(
            user.id,
            user.username,
            user.global_name,
            user.avatar,
        ));
    }
    Err("public profile lookup exhausted its retries".to_owned())
}

fn fetch_official(
    client: &reqwest::blocking::Client,
    id: &str,
    token: &str,
) -> Result<DiscordProfile, String> {
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bot {token}"))
        .map_err(|_| "Discord bot token contains invalid header characters".to_owned())?;
    authorization.set_sensitive(true);
    let user = client
        .get(format!("https://discord.com/api/v10/users/{id}"))
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::json::<DiscordUserResponse>)
        .map_err(|error| format!("Discord API lookup failed: {error}"))?;
    if user.id != id {
        return Err(format!(
            "Discord API returned user {} for requested user {id}",
            user.id
        ));
    }
    Ok(DiscordProfile::from_parts(
        user.id,
        user.username,
        user.global_name,
        user.avatar,
    ))
}

fn unique_ids() -> Vec<&'static str> {
    let mut seen = HashSet::new();
    SECTIONS
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|entry| entry.id))
        .filter(|id| seen.insert(*id))
        .collect()
}

fn avatar_url(id: &str, avatar: Option<&str>) -> String {
    match avatar.filter(|hash| {
        !hash.is_empty()
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }) {
        Some(hash) => {
            format!("https://cdn.discordapp.com/avatars/{id}/{hash}.png?size=256")
        }
        None => default_avatar_url(id),
    }
}

fn default_avatar_url(id: &str) -> String {
    let index = id
        .parse::<u64>()
        .map(|snowflake| (snowflake >> 22) % 6)
        .unwrap_or(0);
    format!("https://cdn.discordapp.com/embed/avatars/{index}.png")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_roster_has_expected_sections_and_unique_people() {
        assert_eq!(
            SECTIONS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["Support", "Testing", "Art", "Upstream Development"]
        );
        assert_eq!(unique_ids().len(), 12);
        assert!(SUPPORT.iter().any(|entry| entry.id == "340711847420231691"));
        assert!(UPSTREAM_DEVELOPMENT
            .iter()
            .any(|entry| entry.id == "340711847420231691"));
        assert!(SUPPORT.iter().any(|entry| entry.id == "120364653795606529"));
        assert!(UPSTREAM_DEVELOPMENT
            .iter()
            .any(|entry| entry.id == "120364653795606529"));
        assert!(UPSTREAM_DEVELOPMENT.iter().all(|entry| {
            entry
                .contribution
                .is_some_and(|contribution| !contribution.trim().is_empty())
        }));
    }

    #[test]
    fn credit_suggestions_link_to_the_requested_discord_account() {
        assert_eq!(CREDIT_SUGGESTION_CONTACT_ID, "751250088910913647");
        assert_eq!(
            credit_suggestion_contact_url(),
            "https://discord.com/users/751250088910913647"
        );
    }

    #[test]
    fn parses_public_lookup_and_prefers_display_name() {
        let response: PublicLookupResponse = serde_json::from_str(
            r#"{"user":{"id":"385629478308806666","username":"themostvile","globalName":"Vile","avatar":"b57d1020f294ddfa3c01756fc6a4fe8b"}}"#,
        )
        .unwrap();
        let user = response.user;
        let profile =
            DiscordProfile::from_parts(user.id, user.username, user.global_name, user.avatar);
        assert_eq!(profile.display_name, "Vile");
        assert_eq!(profile.username, "themostvile");
        assert!(profile.avatar_url.contains("/avatars/385629478308806666/"));
    }

    #[test]
    fn absent_or_invalid_avatar_uses_discord_default() {
        let expected_index = (385629478308806666_u64 >> 22) % 6;
        assert_eq!(
            avatar_url("385629478308806666", None),
            format!("https://cdn.discordapp.com/embed/avatars/{expected_index}.png")
        );
        assert_eq!(
            avatar_url("385629478308806666", Some("../../bad")),
            format!("https://cdn.discordapp.com/embed/avatars/{expected_index}.png")
        );
    }

    #[test]
    fn profile_grid_stays_inside_every_supported_window_width() {
        for width in [420.0, 440.0, 640.0, 900.0, 1040.0, 1600.0] {
            let metrics = grid_metrics(width, SUPPORT.len());
            let occupied = metrics.card_width * metrics.columns as f32
                + metrics.gap * metrics.columns.saturating_sub(1) as f32;
            assert!(metrics.columns >= 1 && metrics.columns <= SUPPORT.len());
            assert!(occupied <= width.max(1.0) + 0.01);
            assert!((50.0..=72.0).contains(&metrics.avatar_size));
        }
    }

    #[test]
    fn art_frame_and_profile_stay_inside_every_supported_window_width() {
        for width in [420.0, 440.0, 640.0, 900.0, 1040.0, 1600.0] {
            let frame = art_frame(width, egui::Color32::WHITE);
            let frame_inset = frame.total_margin().sum().x;
            let content_width = (width - frame_inset).max(1.0);
            let (card_width, avatar_size) = art_profile_sizes(content_width);

            assert!(content_width + frame_inset <= width + 0.01);
            assert!(card_width <= content_width);
            assert!(avatar_size <= (card_width - 30.0).max(1.0));
        }
    }

    #[test]
    fn rendered_art_box_stays_inside_the_scroll_viewport() {
        let credits = CreditsWindow::default();
        for width in [440.0, 640.0, 900.0, 1040.0] {
            let context = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 680.0),
                )),
                ..Default::default()
            };
            let mut measured = None;
            let _ = context.run_ui(input, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let clip_rect = ui.clip_rect();
                            credits.draw_section(ui, "Support", SUPPORT);
                            credits.draw_section(ui, "Testing", TESTING);
                            let response = credits.draw_art_section(ui, ART);
                            measured = Some((clip_rect, response.rect));
                        });
                    });
            });

            let (clip_rect, art_rect) = measured.expect("the Art section should be laid out");
            let painted_right = art_rect.right() - f32::from(ART_FRAME_SIDE_GUTTER);
            assert!(
                painted_right <= clip_rect.right() + 0.01,
                "Art box ended at {}, beyond scroll viewport {} at window width {width}",
                painted_right,
                clip_rect.right()
            );
        }
    }

    #[test]
    fn upstream_contribution_notes_wrap_inside_narrow_and_wide_cards() {
        for card_width in [128.0, 180.0, 320.0] {
            let context = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(card_width, 900.0),
                )),
                ..Default::default()
            };
            let mut note_rects = Vec::new();
            let mut clip_rect = None;
            let _ = context.run_ui(input, |ui| {
                clip_rect = Some(ui.clip_rect());
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(8, 8))
                    .show(ui, |ui| {
                        ui.set_width((card_width - 16.0).max(1.0));
                        for entry in UPSTREAM_DEVELOPMENT {
                            let contribution = entry
                                .contribution
                                .expect("every upstream entry has a contribution note");
                            note_rects.push(add_contribution_note(ui, contribution).rect);
                        }
                    });
            });

            let clip_rect = clip_rect.expect("the test UI should have a clip rectangle");
            let content_width = (card_width - 16.0).max(1.0);
            for rect in note_rects {
                assert!(
                    rect.width() <= content_width + 0.01,
                    "note width {} exceeded card content width {} at card width {card_width}",
                    rect.width(),
                    content_width
                );
                assert!(
                    rect.left() >= clip_rect.left() - 0.01
                        && rect.right() <= clip_rect.right() + 0.01,
                    "note rect {rect:?} escaped the viewport at card width {card_width}"
                );
            }
        }
    }

    #[test]
    fn profile_grid_adds_columns_as_space_grows_without_orphan_rows() {
        let narrow = grid_metrics(440.0, SUPPORT.len());
        let medium = grid_metrics(640.0, SUPPORT.len());
        let wide = grid_metrics(1040.0, SUPPORT.len());
        assert!(narrow.columns <= medium.columns);
        assert!(medium.columns <= wide.columns);
        for metrics in [narrow, medium, wide] {
            let remainder = SUPPORT.len() % metrics.columns;
            assert!(remainder == 0 || remainder * 2 >= metrics.columns);
        }
    }
}
