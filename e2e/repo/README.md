# Workspace-root variants

Each subfolder here is a HOST directory bind-mounted into a sandbox as its
workspace root (the Docker backend treats `--workspace-root` as a host path).
Add a variant by creating a subfolder, e.g. `special_case_b/`, then select it
with `E2E_WORKSPACE_VARIANT=special_case_b` or `config.workspace_variant("special_case_b")`.
The default variant is `testbed/`.
