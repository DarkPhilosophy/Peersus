//! Native SCM per-user service (not LocalSystem). The administrator installs the template once;
//! each account opts in independently. A signed-out account has no service instance.
use crate::instance::Instance;
use std::{ffi::OsString, io, path::Path, sync::OnceLock, time::Duration};
use tokio::sync::watch;
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceStartType,
        ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const TEMPLATE: &str = "PeersusSession";
const PREFERENCE: &str = r"Software\Peersus";
const VALUE: &str = "StartAtSignIn";
const NO_TEMPLATE: &str = "The Windows per-user service template is missing or disabled. An administrator must install it before an account can opt in.";
const PREFERENCE_ERROR: &str = "Windows could not read or save this account's service choice.";

type Owner = fn() -> Result<(), String>;
static OWNER: OnceLock<Owner> = OnceLock::new();
static STOP: OnceLock<watch::Receiver<bool>> = OnceLock::new();

#[derive(Clone)]
pub struct Manager;
pub struct Status {
    pub enabled: bool,
}

fn configured(name: &str) -> Result<bool, String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|_| "The Windows service manager could not be reached.")?;
    let service = manager
        .open_service(name, ServiceAccess::QUERY_CONFIG)
        .map_err(|_| NO_TEMPLATE)?;
    let config = service.query_config().map_err(|_| NO_TEMPLATE)?;
    Ok(config.service_type.contains(ServiceType::USER_OWN_PROCESS)
        && config.start_type == ServiceStartType::AutoStart)
}

fn opted_in() -> Result<bool, String> {
    let value = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(PREFERENCE)
        .and_then(|key| key.get_value::<u32, _>(VALUE));
    match value {
        Ok(1) => Ok(true),
        Ok(0) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        _ => Err(PREFERENCE_ERROR.to_owned()),
    }
}

impl Manager {
    pub fn detect() -> Result<Self, String> {
        Ok(Self)
    }
    pub fn status(&self) -> Result<Status, String> {
        if !configured(TEMPLATE)? {
            return Err(NO_TEMPLATE.to_owned());
        }
        Ok(Status {
            enabled: opted_in()?,
        })
    }
    pub fn enable(&self, _: &Path) -> Result<(), String> {
        if !configured(TEMPLATE)? {
            return Err(NO_TEMPLATE.to_owned());
        }
        Self::write_choice(true)
    }
    pub fn disable(&self) -> Result<(), String> {
        Self::write_choice(false)
    }
    fn write_choice(on: bool) -> Result<(), String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(PREFERENCE)
            .map_err(|_| PREFERENCE_ERROR)?;
        key.set_value(VALUE, &u32::from(on))
            .map_err(|_| PREFERENCE_ERROR.to_owned())
    }
}

/// A console invocation cannot become a service. SCM supplies its actual per-logon instance name.
pub fn dispatch(owner: Owner) -> Result<(), String> {
    OWNER
        .set(owner)
        .map_err(|_| "Windows service dispatcher already started")?;
    service_dispatcher::start(TEMPLATE, service_entry)
        .map_err(|_| "Windows must start the session service through its service manager".into())
}

define_windows_service!(service_entry, service_main);
fn service_main(args: Vec<OsString>) {
    let Some(name) = args.first().and_then(|name| name.to_str()) else {
        return;
    };
    let (stop, stopped) = watch::channel(false);
    let handler = move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop.send_replace(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(handle) = service_control_handler::register(name, handler) else {
        return;
    };
    let report = |state, controls, code| {
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::USER_OWN_PROCESS,
            current_state: state,
            controls_accepted: controls,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            process_id: None,
        })
    };
    let result = (|| -> Result<(), String> {
        // Do not run the application in a machine service, even if an installer misconfigured it.
        if !name
            .strip_prefix("PeersusSession_")
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()))
            || !configured(name)?
        {
            return Err("Not a signed-in user's Windows service instance".into());
        }
        if !opted_in()? {
            return Ok(());
        }
        STOP.set(stopped)
            .map_err(|_| "The service is already running")?;
        report(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
        )
        .map_err(|_| "Windows refused the service start")?;
        OWNER.get().ok_or("The service has no owner")?()
    })();
    let _ = report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        u32::from(result.is_err()),
    );
}

pub async fn stopping() {
    let Some(mut stop) = STOP.get().cloned() else {
        return std::future::pending().await;
    };
    let _ = stop.wait_for(|stopping| *stopping).await;
}

pub async fn acquire(directory: &Path) -> Result<Option<Instance>, String> {
    if STOP.get().is_none() {
        return Err("Windows must start the session service through its service manager".into());
    }
    loop {
        if !opted_in()? {
            return Ok(None);
        }
        if let Some(guard) = Instance::acquire(directory)
            .map_err(|_| "Cannot acquire the application's private directory")?
        {
            return Ok(Some(guard));
        }
        tokio::select! { () = stopping() => return Ok(None), () = tokio::time::sleep(Duration::from_secs(2)) => {} }
    }
}

/// Administrator entry point, not called by the window. Service binaries run on other accounts'
/// logons, so a portable or per-user installation may not register one machine-wide.
pub fn install_template() -> Result<(), String> {
    use windows_service::service::{ServiceErrorControl, ServiceInfo};
    use winreg::enums::HKEY_LOCAL_MACHINE;
    const LOCATION: &str =
        "Install the desktop programs in the machine's Program Files/Peersus directory first.";
    let program_files: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion")
        .and_then(|key| key.get_value("ProgramFilesDir"))
        .map_err(|_| LOCATION)?;
    let program_files = Path::new(&program_files);
    let expected = program_files
        .canonicalize()
        .map_err(|_| LOCATION)?
        .join("Peersus")
        .join("peersus.exe");
    let actual = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|_| LOCATION)?;
    if actual != expected || !actual.is_file() {
        return Err(LOCATION.into());
    }
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(
        |_| "Installing the Windows per-user service template requires administrator approval.",
    )?;
    manager.create_service(&ServiceInfo {
        name: TEMPLATE.into(), display_name: "Peersus (user session)".into(),
        service_type: ServiceType::USER_OWN_PROCESS, start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: program_files.join("Peersus").join("peersus.exe"),
        launch_arguments: vec!["owner".into(), "--service".into()], dependencies: Vec::new(),
        account_name: None, account_password: None,
    }, ServiceAccess::QUERY_CONFIG).map_err(|_| "Windows did not create the session service template. An existing registration was not replaced.")?;
    Ok(())
}
