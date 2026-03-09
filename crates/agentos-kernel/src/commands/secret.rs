use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
    ) -> KernelResponse {
        match self.vault.set(&name, &value, SecretOwner::Kernel, scope) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_secrets(&self) -> KernelResponse {
        match self.vault.list() {
            Ok(list) => KernelResponse::SecretList(list),
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_rotate_secret(&self, name: String, new_value: String) -> KernelResponse {
        match self.vault.rotate(&name, &new_value) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_revoke_secret(&self, name: String) -> KernelResponse {
        match self.vault.revoke(&name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }
}
