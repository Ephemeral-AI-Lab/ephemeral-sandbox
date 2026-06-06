use std::sync::Arc;

use schemars::schema_for;

use crate::core::name::ToolName;
use crate::core::result::OutputShape;
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::json_spec;
use crate::registry::ToolRegistry;

use super::super::super::register_tool;
use super::super::{
    edit_file::{EditFile, EditFileInput},
    exec_command::{ExecCommand, ExecCommandInput},
    multi_edit::{MultiEdit, MultiEditInput},
    read_file::{ReadFile, ReadFileInput},
    write_file::{WriteFile, WriteFileInput},
    write_stdin::{WriteStdin, WriteStdinInput},
};
use super::outputs::{CommandToolOutput, MutationOutput, ReadFileOutput};

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    sandbox_service: super::super::super::SandboxToolService,
    command_service: super::super::super::CommandToolService,
) {
    let read_file = config.get(ToolName::ReadFile);
    register_tool(
        registry,
        ToolName::ReadFile,
        read_file,
        json_spec(
            ToolName::ReadFile,
            &read_file.description,
            schema_for!(ReadFileInput),
            schema_for!(ReadFileOutput),
        ),
        OutputShape::json::<ReadFileOutput>("ReadFileOutput"),
        Arc::new(ReadFile::new(sandbox_service.clone())),
    );
    let write_file = config.get(ToolName::WriteFile);
    register_tool(
        registry,
        ToolName::WriteFile,
        write_file,
        json_spec(
            ToolName::WriteFile,
            &write_file.description,
            schema_for!(WriteFileInput),
            schema_for!(MutationOutput),
        ),
        OutputShape::json::<MutationOutput>("WriteFileOutput"),
        Arc::new(WriteFile::new(sandbox_service.clone())),
    );
    let edit_file = config.get(ToolName::EditFile);
    register_tool(
        registry,
        ToolName::EditFile,
        edit_file,
        json_spec(
            ToolName::EditFile,
            &edit_file.description,
            schema_for!(EditFileInput),
            schema_for!(MutationOutput),
        ),
        OutputShape::json::<MutationOutput>("EditFileOutput"),
        Arc::new(EditFile::new(sandbox_service.clone())),
    );
    let multi_edit = config.get(ToolName::MultiEdit);
    register_tool(
        registry,
        ToolName::MultiEdit,
        multi_edit,
        json_spec(
            ToolName::MultiEdit,
            &multi_edit.description,
            schema_for!(MultiEditInput),
            schema_for!(MutationOutput),
        ),
        OutputShape::json::<MutationOutput>("MultiEditOutput"),
        Arc::new(MultiEdit::new(sandbox_service)),
    );
    let exec_command = config.get(ToolName::ExecCommand);
    register_tool(
        registry,
        ToolName::ExecCommand,
        exec_command,
        json_spec(
            ToolName::ExecCommand,
            &exec_command.description,
            schema_for!(ExecCommandInput),
            schema_for!(CommandToolOutput),
        ),
        OutputShape::json::<CommandToolOutput>("CommandToolOutput"),
        Arc::new(ExecCommand::new(command_service.clone())),
    );
    let write_stdin = config.get(ToolName::WriteStdin);
    register_tool(
        registry,
        ToolName::WriteStdin,
        write_stdin,
        json_spec(
            ToolName::WriteStdin,
            &write_stdin.description,
            schema_for!(WriteStdinInput),
            schema_for!(CommandToolOutput),
        ),
        OutputShape::json::<CommandToolOutput>("CommandToolOutput"),
        Arc::new(WriteStdin::new(command_service)),
    );
}
