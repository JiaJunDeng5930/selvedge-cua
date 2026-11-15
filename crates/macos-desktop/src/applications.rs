use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    bundle_id: String,
    name: String,
    pids: Vec<i32>,
}

/// Lists user-facing running apps and bundles in the standard Applications folders.
pub fn list_apps() -> Vec<App> {
    autoreleasepool(|_| {
        let mut apps = BTreeMap::new();
        for application in NSWorkspace::sharedWorkspace()
            .runningApplications()
            .to_vec()
        {
            if application.activationPolicy() != NSApplicationActivationPolicy::Regular {
                continue;
            }
            let Some(bundle_id) = application.bundleIdentifier().map(|id| id.to_string()) else {
                continue;
            };
            let app = apps.entry(bundle_id.clone()).or_insert_with(|| App {
                name: application
                    .localizedName()
                    .map_or_else(|| bundle_id.clone(), |name| name.to_string()),
                bundle_id,
                pids: Vec::new(),
            });
            app.pids.push(application.processIdentifier());
        }

        let system = [
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
        ];
        let user = env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications"));
        for directory in system.into_iter().chain(user) {
            collect_bundles(&directory, &mut apps);
        }
        apps.into_values().collect()
    })
}

fn collect_bundles(directory: &Path, apps: &mut BTreeMap<String, App>) {
    // DEFENSE: Standard application folders can be absent or contain
    // unreadable entries. Discovery reports the bundles it can inspect.
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        {
            let url = NSURL::from_file_path(&path).expect("directory entries contain no NUL");
            let Some(bundle_id) = NSBundle::bundleWithURL(&url)
                .and_then(|bundle| bundle.bundleIdentifier())
                .map(|id| id.to_string())
            else {
                continue;
            };
            apps.entry(bundle_id.clone()).or_insert_with(|| App {
                name: path
                    .file_stem()
                    .expect("an app name has a stem")
                    .to_string_lossy()
                    .into_owned(),
                bundle_id,
                pids: Vec::new(),
            });
        } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_bundles(&path, apps);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningApp {
    bundle_id: String,
    pid: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchApp {
    bundle_id: Option<String>,
    name: Option<String>,
}

impl LaunchApp {
    pub fn bundle_id(self) -> Result<String, String> {
        match (self.bundle_id, self.name) {
            (Some(bundle_id), None) if !bundle_id.is_empty() => Ok(bundle_id),
            (None, Some(name)) if !name.is_empty() => {
                let matches: Vec<_> = list_apps()
                    .into_iter()
                    .filter(|app| app.name.eq_ignore_ascii_case(&name))
                    .map(|app| app.bundle_id)
                    .collect();
                match matches.as_slice() {
                    [bundle_id] => Ok(bundle_id.clone()),
                    [] => Err(format!("No application named {name} in desktop.listApps")),
                    _ => Err(format!(
                        "Application name {name} matches {}; specify bundleId",
                        matches.join(", ")
                    )),
                }
            }
            _ => Err("desktop.launchApp requires either bundleId or name".into()),
        }
    }
}

/// Asks Launch Services to open the registered app without activating it.
pub async fn launch_app(bundle_id: String) -> Result<RunningApp, String> {
    let (sender, receiver) = oneshot::channel();
    let running = autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let identifier = NSString::from_str(&bundle_id);
        let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
        if running.count() == 1 {
            let app = running.objectAtIndex(0);
            if !app.isTerminated() {
                return Ok(Some(RunningApp {
                    bundle_id,
                    pid: app.processIdentifier(),
                }));
            }
        }
        let url = workspace
            .URLForApplicationWithBundleIdentifier(&identifier)
            .ok_or_else(|| format!("No registered application with bundleId {bundle_id}"))?;
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        configuration.setActivates(false);
        configuration.setAddsToRecentItems(false);
        configuration.setPromptsUserIfNeeded(false);
        let sender = std::sync::Mutex::new(Some(sender));
        let completion =
            RcBlock::new(move |app: *mut NSRunningApplication, error: *mut NSError| {
                let Some(sender) = sender.lock().expect("application completion lock").take()
                else {
                    return;
                };
                // SAFETY: AppKit passes live objects for the duration of the callback.
                let result = unsafe {
                    match (app.as_ref(), error.as_ref()) {
                        (_, Some(error)) => Err(error.localizedDescription().to_string()),
                        (Some(app), None) => Ok(RunningApp {
                            bundle_id: bundle_id.clone(),
                            pid: app.processIdentifier(),
                        }),
                        // DEFENSE: No app was returned, so there is no PID to target.
                        (None, None) => Err("Application launch returned no process".into()),
                    }
                };
                let _ = sender.send(result);
            });
        workspace.openApplicationAtURL_configuration_completionHandler(
            &url,
            &configuration,
            Some(&completion),
        );
        Ok::<_, String>(None)
    })?;
    if let Some(running) = running {
        return Ok(running);
    }
    receiver
        .await
        .map_err(|_| "Application launch completion was lost")?
}
