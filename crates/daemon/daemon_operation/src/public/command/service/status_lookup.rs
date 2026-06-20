use super::core::CommandOperationService;

use crate::command::{CommandServiceError, CompletedCommandRecord, FinalizationState};

impl CommandOperationService {
    pub(crate) fn active_command<'a>(
        &'a self,
        command_session_id: &crate::command::CommandSessionId,
    ) -> Result<crate::command::ActiveCommandRef<'a>, CommandServiceError> {
        match self.active_command_or_none(command_session_id)? {
            Some(active) => Ok(active),
            None => match self.process_store().completed(command_session_id) {
                Some(_) => Err(CommandServiceError::CommandAlreadyCompleted {
                    command_session_id: command_session_id.clone(),
                }),
                None => Err(CommandServiceError::CommandNotFound {
                    command_session_id: command_session_id.clone(),
                }),
            },
        }
    }

    pub(crate) fn active_command_or_none<'a>(
        &'a self,
        command_session_id: &crate::command::CommandSessionId,
    ) -> Result<Option<crate::command::ActiveCommandRef<'a>>, CommandServiceError> {
        let Some(active) = self.process_store().active(command_session_id) else {
            return Ok(None);
        };
        if let FinalizationState::Failed { error, finalized } = &active.finalization {
            return Err(CommandServiceError::CommandFinalizationFailed {
                command_session_id: command_session_id.clone(),
                error: error.clone(),
                finalized: finalized.clone().map(Box::new),
            });
        }
        Ok(Some(active))
    }

    pub(crate) fn completed_command(
        &self,
        command_session_id: &crate::command::CommandSessionId,
    ) -> Result<CompletedCommandRecord, CommandServiceError> {
        self.process_store()
            .completed(command_session_id)
            .ok_or_else(|| CommandServiceError::CommandNotFound {
                command_session_id: command_session_id.clone(),
            })
    }

    pub(crate) fn ensure_active_command(
        &self,
        command_session_id: &crate::command::CommandSessionId,
    ) -> Result<(), CommandServiceError> {
        drop(self.active_command(command_session_id)?);
        Ok(())
    }
}
