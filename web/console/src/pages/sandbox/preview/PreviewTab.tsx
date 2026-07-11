import { useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router";
import { ExternalLink, RotateCw } from "lucide-react";
import { Button, Input, Loader, Select } from "@mantine/core";
import { useSandbox } from "@/pages/sandbox/SandboxContext";
import { previewScopes } from "@/pages/sandbox/SandboxHeader";

/**
 * The embedded web viewer: an iframe over the console's `/s/:id/...`
 * preview proxy. The frame is deliberately opaque to the Console: scope,
 * port, and initial path live in the query string so launchers can deep-link
 * without reading or synchronizing in-frame navigation.
 */
export function PreviewTab() {
  const { sandboxId, snapshot } = useSandbox();
  const [searchParams, setSearchParams] = useSearchParams();
  const scope = searchParams.get("scope") ?? "shared";
  const port = searchParams.get("port") ?? "";
  const path = searchParams.get("path") ?? "/";
  const [pathDraft, setPathDraft] = useState(path);
  const [portDraft, setPortDraft] = useState(port);
  const [reloadKey, setReloadKey] = useState(0);
  const [isLoading, setIsLoading] = useState(false);

  useEffect(() => setPathDraft(path), [path]);
  useEffect(() => setPortDraft(port), [port]);

  const scopes = previewScopes(snapshot ?? undefined);
  const scopeEntry = scopes.find((entry) => entry.id === scope);
  const isolated = scope !== "shared";

  const portValid = /^\d+$/.test(port) && Number(port) >= 1 && Number(port) <= 65535;
  const previewUrl = useMemo(() => {
    if (!portValid) return null;
    const scopeSegment = scope === "shared" ? "shared" : `isolated=${scope}`;
    const cleanPath = path.startsWith("/") ? path.slice(1) : path;
    return `/s/${encodeURIComponent(sandboxId)}/${scopeSegment}/${port}/${cleanPath}`;
  }, [sandboxId, scope, port, path, portValid]);

  useEffect(() => {
    setIsLoading(Boolean(previewUrl));
  }, [previewUrl, reloadKey]);

  const apply = (next: { scope?: string; port?: string; path?: string }) => {
    const params = new URLSearchParams(searchParams);
    if (next.scope !== undefined) params.set("scope", next.scope);
    if (next.port !== undefined) params.set("port", next.port);
    if (next.path !== undefined) params.set("path", next.path);
    setSearchParams(params, { replace: true });
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-line bg-surface px-3 py-2">
        <label className="text-[11px] text-ink-faint">scope</label>
        <Select
          className="w-52"
          value={scope}
          onChange={(value) => apply({ scope: value ?? "shared" })}
          data={[
            ...scopes.map((entry) => ({ value: entry.id, label: entry.label })),
            ...(scopeEntry || scope === "shared" ? [] : [{ value: scope, label: `isolated · ${scope}` }]),
          ]}
        />
        <label className="text-[11px] text-ink-faint" htmlFor="preview-port">
          port
        </label>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            apply({ port: portDraft.trim() });
          }}
        >
          <Input
            id="preview-port"
            value={portDraft}
            onChange={(event) => setPortDraft(event.target.value)}
            placeholder="5173"
            className="w-20 font-mono"
            inputMode="numeric"
          />
        </form>
        <label className="text-[11px] text-ink-faint" htmlFor="preview-path">
          path
        </label>
        <form
          className="min-w-0 flex-1"
          onSubmit={(event) => {
            event.preventDefault();
            apply({ path: pathDraft.trim() === "" ? "/" : pathDraft.trim() });
          }}
        >
          <Input
            id="preview-path"
            value={pathDraft}
            onChange={(event) => setPathDraft(event.target.value)}
            placeholder="/"
            className="w-full font-mono"
          />
        </form>
        <Button
          size="compact-xs"
          onClick={() => setReloadKey((key) => key + 1)}
          disabled={!previewUrl}
          title="refresh"
        >
          <RotateCw size={12} />
        </Button>
        <Button
          size="compact-xs"
          onClick={() => {
            if (previewUrl) window.open(previewUrl, "_blank", "noopener,noreferrer");
          }}
          disabled={!previewUrl}
          title="open in a new tab"
        >
          <ExternalLink size={12} />
          tab
        </Button>
      </div>

      {isolated ? (
        <div className="border-b border-warn/40 bg-warn-soft px-3 py-1 text-[11px] text-ink">
          Isolated session: the server must bind <span className="font-mono">0.0.0.0</span>{" "}
          or the workspace IP — <span className="font-mono">127.0.0.1</span> is not
          reachable from outside the namespace.
        </div>
      ) : null}

      <div className="relative min-h-0 flex-1 bg-app" aria-busy={isLoading}>
        {previewUrl ? (
          <>
            {isLoading ? (
              <div className="absolute top-0 w-full flex items-center gap-2 bg-surface/90 px-3 py-1.5 text-[11px] text-ink-mid" role="status">
                <Loader size="xs" />
                Loading preview…
              </div>
            ) : null}
            <iframe
              key={`${previewUrl}-${reloadKey}`}
              src={previewUrl}
              onLoad={() => setIsLoading(false)}
              sandbox="allow-scripts"
              allow=""
              referrerPolicy="no-referrer"
              title={`preview of ${sandboxId} port ${port}`}
              className="h-full w-full border-0 bg-white"
            />
          </>
        ) : (
          <div className="mx-auto mt-16 max-w-md rounded-lg border border-line bg-surface p-8 text-center">
            <div className="text-sm font-semibold">Pick a port to preview</div>
            <p className="mt-2 text-xs text-ink-mid">
              Anything serving HTTP inside the sandbox renders here through{" "}
              <span className="font-mono">/s/{sandboxId}/…</span> — start a
              server in the Terminal tab, then enter its port above.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
