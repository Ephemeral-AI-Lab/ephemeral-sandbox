import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Layers } from "lucide-react";
import { rpcStream, systemScope } from "@/api/rpc";
import { useErrorToast } from "@/components/ErrorToast";
import { StreamLogPane } from "@/components/StreamLogPane";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface SquashResult {
  layers_before?: number;
  layers_after?: number;
  [key: string]: unknown;
}

/**
 * checkpoint_squash with `_stream_logs` into a StreamLogPane. A pre-run
 * "est. after" count is not derivable from any op, so the dialog shows the
 * before-count only and reports the after-count from the result.
 */
export function SquashDialog({
  sandboxId,
  layerCount,
  trigger,
}: {
  sandboxId: string;
  layerCount?: number;
  trigger: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [logs, setLogs] = useState<string[]>([]);
  const [result, setResult] = useState<SquashResult | null>(null);
  const { showError } = useErrorToast();
  const queryClient = useQueryClient();

  const run = async () => {
    setBusy(true);
    setLogs([]);
    setResult(null);
    try {
      const outcome = await rpcStream<SquashResult>(
        "checkpoint_squash",
        systemScope,
        { sandbox_id: sandboxId },
        (line) => setLogs((current) => [...current, line]),
      );
      setResult(outcome);
      void queryClient.invalidateQueries({ queryKey: ["fleet"] });
      void queryClient.invalidateQueries({ queryKey: ["sandbox", sandboxId] });
    } catch (error) {
      showError(error);
      setOpen(false);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (busy) return;
        setOpen(next);
        if (next) {
          setLogs([]);
          setResult(null);
        }
      }}
    >
      {trigger}
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-1.5">
            <Layers size={14} className="text-accent" />
            Squash layer stack
          </DialogTitle>
          <DialogDescription>
            Squashes every squashable block of{" "}
            <span className="font-mono text-ink">{sandboxId}</span>&apos;s
            published layers and live-remounts its sessions.
            {typeof layerCount === "number" ? (
              <> Current stack: {layerCount} layers.</>
            ) : null}
          </DialogDescription>
        </DialogHeader>
        {busy || logs.length > 0 ? <StreamLogPane lines={logs} /> : null}
        {result ? (
          <div className="mt-2 rounded border border-ok/40 bg-ok-soft p-2 font-mono text-xs text-ink">
            {JSON.stringify(result)}
          </div>
        ) : null}
        <div className="mt-4 flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setOpen(false)} disabled={busy}>
            Close
          </Button>
          <Button variant="primary" onClick={() => void run()} disabled={busy}>
            {busy ? "Squashing…" : "Squash"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
