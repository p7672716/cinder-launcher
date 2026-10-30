use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::prelude::FluentBuilder;
use std::path::PathBuf;

use cinder_launcher::{
    api_credentials::{self, PublicSettings, SecretKind, SecretStore},
    instance::{GameTarget, Instance, Loader},
    instance_store::InstanceStore,
    microsoft_auth::OwnedSession,
    p3_flow, settings,
    vanilla_launch::JavaChoice,
};
use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    theme::{Theme, ThemeMode},
};
use gpui_kit::*;
use uuid::Uuid;

struct Home {
    font_input: Entity<InputState>,
    config_path: PathBuf,
    message: String,
    current_font: String,
    store: InstanceStore,
    instances: Vec<Instance>,
    busy: bool,
    new_name: Entity<InputState>,
    minecraft_version: Entity<InputState>,
    loader_version: Entity<InputState>,
    selected_loader: Loader,
    selected: Option<Uuid>,
    rename_name: Entity<InputState>,
    services_path: PathBuf,
    secrets: SecretStore,
    client_id_input: Entity<InputState>,
    curseforge_key_input: Entity<InputState>,
    modrinth_token_input: Entity<InputState>,
    configured_client_id: bool,
    curseforge_saved: Option<bool>,
    modrinth_saved: Option<bool>,
    services_busy: bool,
    services_message: String,
    session: Option<OwnedSession>,
    auth_busy: bool,
    auth_message: String,
    game_busy: bool,
    game_message: String,
    java_busy: bool,
    java_path: Entity<InputState>,
    java_message: String,
}

impl Home {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        path: PathBuf,
        font: String,
        effective: String,
        store: InstanceStore,
    ) -> Self {
        let new_name = cx.new(|cx| InputState::new(window, cx).placeholder("インスタンス名"));
        let minecraft_version = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Minecraft バージョン")
                .default_value("1.21.1")
        });
        let loader_version =
            cx.new(|cx| InputState::new(window, cx).placeholder("NeoForge ビルド（任意）"));
        let rename_name = cx.new(|cx| InputState::new(window, cx).placeholder("変更後の名前"));
        let mut home = Self {
            font_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("フォント名")
                    .default_value(font)
            }),
            config_path: path,
            current_font: effective,
            message: String::new(),
            store,
            instances: Vec::new(),
            busy: false,
            new_name,
            minecraft_version,
            loader_version,
            selected_loader: Loader::NeoForge { version: None },
            selected: None,
            rename_name,
            services_path: api_credentials::config_path()
                .expect("サービス設定パスを取得できません"),
            secrets: SecretStore::default(),
            client_id_input: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Microsoft クライアントID（GUID）")
            }),
            curseforge_key_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("CurseForge APIキーを入力")
                    .masked(true)
            }),
            modrinth_token_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Modrinth トークン（任意）")
                    .masked(true)
            }),
            configured_client_id: false,
            curseforge_saved: None,
            modrinth_saved: None,
            services_busy: false,
            services_message: String::new(),
            session: None,
            auth_busy: false,
            auth_message: "未ログイン".into(),
            game_busy: false,
            game_message: String::new(),
            java_busy: false,
            java_path: cx.new(|cx| InputState::new(window, cx).placeholder("java.exe の絶対パス")),
            java_message: "既定: Mojang提供Java".into(),
        };
        home.refresh(cx);
        home.refresh_services(cx);
        home
    }

    // All filesystem work runs on the background executor, never inside a click handler.
    fn run_io<F>(&mut self, cx: &mut Context<Self>, operation: F)
    where
        F: FnOnce(InstanceStore) -> std::io::Result<Vec<Instance>> + Send + 'static,
    {
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = "保存・読込中…".into();
        let store = self.store.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { operation(store) })
                .await;
            if let Some(entity) = weak.upgrade() {
                entity.update(cx, |view, cx| {
                    view.busy = false;
                    match result {
                        Ok(instances) => {
                            view.instances = instances;
                            view.message = "完了しました。".into();
                        }
                        Err(err) => view.message = format!("操作できませんでした: {err}"),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.run_io(cx, |store| store.list());
    }

    fn create_instance(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).value().to_string();
        let minecraft_version = self.minecraft_version.read(cx).value().to_string();
        let loader = match &self.selected_loader {
            Loader::Vanilla => Loader::Vanilla,
            Loader::NeoForge { .. } => {
                let v = self.loader_version.read(cx).value().trim().to_owned();
                Loader::NeoForge {
                    version: if v.is_empty() { None } else { Some(v) },
                }
            }
        };
        self.run_io(cx, move |store| {
            store.create(
                &name,
                GameTarget {
                    minecraft_version,
                    loader,
                },
            )?;
            store.list()
        });
    }

    fn rename_instance(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            self.message = "一覧からインスタンスを選択してください。".into();
            cx.notify();
            return;
        };
        let name = self.rename_name.read(cx).value().to_string();
        self.run_io(cx, move |store| {
            store.rename(id, &name)?;
            store.list()
        });
    }

    // Credential Manager access and JSON writes must not block the GUI thread.
    fn run_services<F>(&mut self, cx: &mut Context<Self>, operation: F)
    where
        F: FnOnce(SecretStore, PathBuf) -> std::io::Result<()> + Send + 'static,
    {
        if self.services_busy {
            return;
        }
        self.services_busy = true;
        self.services_message = "連携設定を確認中…".into();
        let store = self.secrets.clone();
        let path = self.services_path.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    operation(store.clone(), path.clone())?;
                    let configured = PublicSettings::load(&path)?.microsoft_client_id.is_some();
                    let cf = store.is_configured(SecretKind::CurseForge).ok();
                    let mr = store.is_configured(SecretKind::Modrinth).ok();
                    Ok::<_, std::io::Error>((configured, cf, mr))
                })
                .await;
            if let Some(entity) = weak.upgrade() {
                entity.update(cx, |home, cx| {
                    home.services_busy = false;
                    match result {
                        Ok((id, cf, mr)) => {
                            home.configured_client_id = id;
                            home.curseforge_saved = cf;
                            home.modrinth_saved = mr;
                            home.services_message = "連携設定を確認しました。".into();
                        }
                        Err(err) => {
                            home.services_message = format!("連携設定を更新できません: {err}")
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn refresh_services(&mut self, cx: &mut Context<Self>) {
        self.run_services(cx, |_, _| Ok(()));
    }

    fn save_client_id(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.client_id_input.read(cx).value().to_string();
        let mut validate = PublicSettings::default();
        if validate.set_microsoft_id(&id).is_err() {
            self.services_message =
                "クライアントIDはMicrosoftのGUID形式で入力してください。".into();
            cx.notify();
            return;
        }
        if self.services_busy || self.auth_busy || self.game_busy {
            self.services_message = "認証・起動処理の完了後にアプリIDを変更してください。".into();
            cx.notify();
            return;
        }
        self.client_id_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.run_services(cx, move |store, path| {
            let mut settings = PublicSettings::load(&path)?;
            let changed = settings.microsoft_client_id.as_deref()
                != if id.trim().is_empty() {
                    None
                } else {
                    Some(id.trim())
                };
            if changed {
                store.delete(SecretKind::MicrosoftRefresh)?;
            }
            settings.set_microsoft_id(&id)?;
            settings.save(&path)
        });
        self.session = None;
        self.auth_message =
            "MicrosoftクライアントIDを設定した場合は再ログインしてください。".into();
    }

    fn save_curseforge_key(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.save_secret(SecretKind::CurseForge, window, cx);
    }
    fn clear_curseforge_key(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_secret(SecretKind::CurseForge, cx);
    }
    fn save_modrinth_token(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.save_secret(SecretKind::Modrinth, window, cx);
    }
    fn clear_modrinth_token(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_secret(SecretKind::Modrinth, cx);
    }
    fn save_secret(&mut self, kind: SecretKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.services_busy {
            return;
        }
        let input = match kind {
            SecretKind::CurseForge => self.curseforge_key_input.clone(),
            SecretKind::Modrinth => self.modrinth_token_input.clone(),
            SecretKind::MicrosoftRefresh => return,
        };
        let value = input.read(cx).value().to_string();
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            self.services_message =
                "空でないキーを入力してください。削除は「削除」を使用してください。".into();
            cx.notify();
            return;
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.run_services(cx, move |store, _| store.set(kind, &value));
    }
    fn clear_secret(&mut self, kind: SecretKind, cx: &mut Context<Self>) {
        self.run_services(cx, move |store, _| store.delete(kind));
    }

    fn login(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.authenticate(false, cx);
    }
    fn restore_login(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.authenticate(true, cx);
    }
    fn authenticate(&mut self, restore: bool, cx: &mut Context<Self>) {
        if self.auth_busy || self.game_busy || self.services_busy {
            return;
        }
        self.auth_busy = true;
        self.auth_message = if restore {
            "保存済みのMicrosoftログインを確認中…"
        } else {
            "ブラウザーでMicrosoftにサインインしてください…"
        }
        .into();
        let path = self.services_path.clone();
        let secrets = self.secrets.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if restore {
                        p3_flow::restore_session(&path, &secrets)
                    } else {
                        p3_flow::sign_in(&path, &secrets)
                    }
                })
                .await;
            if let Some(view) = weak.upgrade() {
                view.update(cx, |home, cx| {
                    home.auth_busy = false;
                    match result {
                        Ok(session) => {
                            home.auth_message = format!(
                                "ログイン済み: {}（Minecraft所有権を確認済み）",
                                session.name
                            );
                            home.session = Some(session);
                        }
                        Err(error) => {
                            home.auth_message = format!("Microsoftログインに失敗しました: {error}")
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }
    fn logout(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.auth_busy || self.game_busy || self.services_busy {
            return;
        }
        self.auth_busy = true;
        let secrets = self.secrets.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { p3_flow::logout(&secrets) })
                .await;
            if let Some(view) = weak.upgrade() {
                view.update(cx, |home, cx| {
                    home.auth_busy = false;
                    match result {
                        Ok(()) => {
                            home.session = None;
                            home.auth_message = "ログアウトしました。".into();
                        }
                        Err(error) => {
                            home.auth_message = format!("ログアウトできませんでした: {error}")
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn install_vanilla(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.game_busy || self.busy {
            return;
        }
        let Some(id) = self.selected else {
            self.game_message = "インスタンスを選択してください。".into();
            cx.notify();
            return;
        };
        self.game_busy = true;
        self.game_message = "MinecraftとJavaを検証・取得中…".into();
        let store = self.store.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { p3_flow::install(&store, id) })
                .await;
            if let Some(view) = weak.upgrade() {
                view.update(cx, |home, cx| {
                    home.game_busy = false;
                    home.game_message = match result {
                        Ok(()) => "MinecraftとJavaの導入・検証が完了しました。".into(),
                        Err(error) => format!("導入できませんでした: {error}"),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }
    fn launch_vanilla(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.game_busy || self.busy || self.auth_busy {
            return;
        }
        let Some(id) = self.selected else {
            self.game_message = "インスタンスを選択してください。".into();
            cx.notify();
            return;
        };
        if self.session.is_none() {
            self.game_message = "Minecraftアカウントでログインしてください。".into();
            cx.notify();
            return;
        }
        self.game_busy = true;
        self.game_message = "利用権を更新・Minecraftの起動準備中…".into();
        let store = self.store.clone();
        let path = self.services_path.clone();
        let secrets = self.secrets.clone();
        cx.spawn(async move |weak, cx| {
            let start = cx
                .background_executor()
                .spawn(async move {
                    let session = p3_flow::restore_session(&path, &secrets)?;
                    let game = p3_flow::launch(&store, id, &session.into_launch_identity())?;
                    Ok::<_, std::io::Error>((session, game))
                })
                .await;
            match start {
                Ok((session, game)) => {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |home, cx| {
                            home.auth_message =
                                format!("ログイン済み: {}（所有権確認済み）", session.name);
                            home.session = Some(session);
                            home.game_message =
                                "Minecraftのプロセスを起動しました（タイトル画面到達は未確認）。"
                                    .into();
                            cx.notify();
                        });
                    }
                    let result = cx
                        .background_executor()
                        .spawn(async move { game.collect() })
                        .await;
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |home, cx| {
                            home.game_busy = false;
                            home.game_message = match result {
                                Ok(output) if output.status.success() => {
                                    "Minecraftプロセスが正常終了しました。".into()
                                }
                                Ok(output) => format!(
                                    "Minecraftプロセスが終了しました（終了コード: {:?}）。",
                                    output.status.code()
                                ),
                                Err(error) => format!("Minecraft終了処理に失敗: {error}"),
                            };
                            cx.notify();
                        });
                    }
                }
                Err(error) => {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |home, cx| {
                            home.game_busy = false;
                            home.game_message = format!("Minecraftを起動できませんでした: {error}");
                            cx.notify();
                        });
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }
    fn load_java(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if self.java_busy {
            return;
        }
        self.java_busy = true;
        let store = self.store.clone();
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { p3_flow::current_java(&store, id) })
                .await;
            if let Some(view) = weak.upgrade() {
                view.update(cx, |home, cx| {
                    home.java_busy = false;
                    if home.selected == Some(id) {
                        home.java_message = match result {
                            Ok(JavaChoice::Managed) => "Java: Mojang提供（既定）".into(),
                            Ok(JavaChoice::Custom(path)) => format!("Java: {}", path.display()),
                            Err(error) => format!("Java設定を確認できません: {error}"),
                        };
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn save_managed_java(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.save_java_choice(JavaChoice::Managed, cx);
    }
    fn save_custom_java(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.java_path.read(cx).value().to_string();
        if !std::path::Path::new(&path).is_absolute() {
            self.java_message = "Javaの絶対パスを入力してください。".into();
            cx.notify();
            return;
        }
        self.save_java_choice(JavaChoice::Custom(path.into()), cx);
    }
    fn save_java_choice(&mut self, choice: JavaChoice, cx: &mut Context<Self>) {
        if self.java_busy || self.game_busy {
            return;
        }
        let Some(id) = self.selected else {
            self.java_message = "インスタンスを選択してください。".into();
            cx.notify();
            return;
        };
        let store = self.store.clone();
        self.java_busy = true;
        cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { p3_flow::save_java(&store, id, &choice) })
                .await;
            if let Some(view) = weak.upgrade() {
                view.update(cx, |home, cx| {
                    home.java_busy = false;
                    home.java_message = match result {
                        Ok(()) => "Java設定を保存しました。".into(),
                        Err(error) => format!("Java設定を保存できません: {error}"),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn apply_font(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let requested = self.font_input.read(cx).value().to_string();
        if !settings::validate_font(&requested) {
            self.message = "フォント名を入力してください。".into();
            cx.notify();
            return;
        }
        let available = cx.text_system().all_font_names();
        if !available
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&requested))
        {
            self.message =
                "指定されたフォントは見つかりません。インストール済みの書体名を指定してください。"
                    .into();
            cx.notify();
            return;
        }
        if let Err(err) = settings::save(&self.config_path, &requested) {
            self.message = format!("フォント設定を保存できません: {err}");
            cx.notify();
            return;
        }
        self.current_font = requested.clone();
        self.message = "フォント設定を保存しました。".into();
        Theme::update(cx, |theme| theme.font_family = requested.into());
        cx.notify();
    }
}

impl Render for Home {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child("Cinder Launcher"),
            )
            .child(div().font_weight(FontWeight::BOLD).child("インスタンス"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.instances.iter().map(|instance| {
                        let id = instance.id();
                        let selected = self.selected == Some(id);
                        let summary = match &instance.target().loader {
                            Loader::Vanilla => format!(
                                "Minecraft {} · Vanilla",
                                instance.target().minecraft_version
                            ),
                            Loader::NeoForge { version } => format!(
                                "Minecraft {} · NeoForge {}",
                                instance.target().minecraft_version,
                                version.as_deref().unwrap_or("ビルド未指定")
                            ),
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                Button::new(format!("pick-{id}"))
                                    .label(instance.name().to_owned())
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.selected = Some(id);
                                        view.load_java(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(summary),
                            )
                            .when(selected, |this| this.child(div().text_sm().child("選択中")))
                    }))
                    .when(self.instances.is_empty(), |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("インスタンスはまだありません。"),
                        )
                    }),
            )
            .child(
                div()
                    .mt_2()
                    .font_weight(FontWeight::BOLD)
                    .child("新しいインスタンス"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.new_name).aria_label("新しいインスタンス名")),
                    )
                    .child(div().flex_1().child(
                        Input::new(&self.minecraft_version).aria_label("Minecraftバージョン"),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("vanilla")
                            .label("Vanilla")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.selected_loader = Loader::Vanilla;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("neoforge")
                            .label("NeoForge")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.selected_loader = Loader::NeoForge { version: None };
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child(match self.selected_loader {
                        Loader::Vanilla => "Vanilla を使用",
                        _ => "NeoForge を使用",
                    })),
            )
            .when(
                matches!(self.selected_loader, Loader::NeoForge { .. }),
                |this| this.child(Input::new(&self.loader_version).aria_label("NeoForgeビルド")),
            )
            .child(
                Button::new("create-instance")
                    .primary()
                    .label("作成")
                    .on_click(cx.listener(Self::create_instance)),
            )
            .child(
                div()
                    .mt_2()
                    .font_weight(FontWeight::BOLD)
                    .child("選択したインスタンスの名前変更"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Input::new(&self.rename_name).aria_label("変更後のインスタンス名"),
                        ),
                    )
                    .child(
                        Button::new("rename-instance")
                            .label("名前変更")
                            .on_click(cx.listener(Self::rename_instance)),
                    ),
            )
            .child(div().mt_4().font_weight(FontWeight::BOLD).child("Minecraft Vanilla"))
            .child(div().text_sm().child(self.game_message.clone()))
            .child(div().flex().gap_2()
                .child(Button::new("install-vanilla").label("導入・ファイル検証").on_click(cx.listener(Self::install_vanilla)))
                .child(Button::new("launch-vanilla").primary().label("起動").on_click(cx.listener(Self::launch_vanilla))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("初回導入ではMinecraftの公式ファイルをダウンロードします。ゲームは正規アカウントが必要です。"))
            .child(div().mt_2().child("インスタンスのJava設定"))
            .child(div().text_sm().child(self.java_message.clone()))
            .child(div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.java_path).aria_label("Java実行ファイルの絶対パス")))
                .child(Button::new("save-custom-java").label("手動Javaを保存").on_click(cx.listener(Self::save_custom_java)))
                .child(Button::new("save-managed-java").label("Mojang提供Java").on_click(cx.listener(Self::save_managed_java))))
            .child(div().mt_2().font_weight(FontWeight::BOLD).child("Minecraftアカウント"))
            .child(div().text_sm().child(self.auth_message.clone()))
            .child(div().flex().gap_2()
                .child(Button::new("sign-in-microsoft").primary().label("Microsoftログイン").on_click(cx.listener(Self::login)))
                .child(Button::new("restore-microsoft").label("保存したログインを復元").on_click(cx.listener(Self::restore_login)))
                .child(Button::new("sign-out-microsoft").label("ログアウト").on_click(cx.listener(Self::logout))))
            .child(div().mt_4().child("表示フォント"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.font_input).aria_label("表示フォント名")),
                    )
                    .child(
                        Button::new("save-font")
                            .primary()
                            .label("適用して保存")
                            .on_click(cx.listener(Self::apply_font)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("現在のフォント: {}", self.current_font)),
            )
            .child(div().mt_4().font_weight(FontWeight::BOLD).child("連携サービス設定"))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("MicrosoftクライアントIDは公開識別子です。APIキーはWindows資格情報マネージャーに保存します。"))
            .child(div().text_sm().child(format!("Microsoft アプリID: {}",if self.configured_client_id {"設定済み"} else {"未設定"})))
            .child(div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.client_id_input).aria_label("MicrosoftクライアントID")))
                .child(Button::new("save-client-id").label("保存／空欄で削除").on_click(cx.listener(Self::save_client_id))))
            .child(div().text_sm().child(format!("CurseForge APIキー: {}",match self.curseforge_saved {Some(true)=>"保存済み",Some(false)=>"未設定",None=>"Windowsで利用可能"})))
            .child(div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.curseforge_key_input).aria_label("CurseForge APIキー").content_type(gpui_kit::component::input::InputContentType::Password)))
                .child(Button::new("save-curseforge-key").label("保存").on_click(cx.listener(Self::save_curseforge_key)))
                .child(Button::new("clear-curseforge-key").label("削除").on_click(cx.listener(Self::clear_curseforge_key))))
            .child(div().text_sm().child(format!("Modrinth 個人用トークン（通常不要）: {}",match self.modrinth_saved {Some(true)=>"保存済み",Some(false)=>"未設定",None=>"Windowsで利用可能"})))
            .child(div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.modrinth_token_input).aria_label("Modrinthトークン").content_type(gpui_kit::component::input::InputContentType::Password)))
                .child(Button::new("save-modrinth-token").label("保存").on_click(cx.listener(Self::save_modrinth_token)))
                .child(Button::new("clear-modrinth-token").label("削除").on_click(cx.listener(Self::clear_modrinth_token))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("保存済み表示はキーの有効性やサービス接続成功を保証しません。"))
            .child(div().text_sm().child(self.services_message.clone()))
            .child(div().text_sm().child(self.message.clone()))
            .overflow_y_scrollbar()
    }
}

fn main() {
    let path = settings::config_path().expect("設定ディレクトリを取得できません");
    let store = InstanceStore::new(
        InstanceStore::default_path().expect("インスタンス用ディレクトリを取得できません"),
    );
    let saved = settings::load(&path).unwrap_or_else(|err| {
        eprintln!("フォント設定を読み込めません: {err}");
        settings::DEFAULT_FONT.into()
    });

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let available = cx.text_system().all_font_names();
        let effective = settings::resolve_font(&saved, &available).to_owned();
        Theme::update(cx, |theme| theme.font_family = effective.clone().into());
        let path = path.clone();
        let saved = saved.clone();
        let store = store.clone();
        open_window(WindowOptions::default(), cx, move |window, cx| {
            window.set_window_title("Cinder Launcher");
            cx.new(|cx| Home::new(window, cx, path, saved, effective, store))
        })
        .expect("ウィンドウを開けません");
    });
}
