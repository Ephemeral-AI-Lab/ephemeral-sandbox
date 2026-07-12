import { Component, type ErrorInfo, type ReactNode } from "react";
import { Alert, MantineProvider } from "@mantine/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AlertTriangle } from "lucide-react";
import { benchmarkTheme } from "@/theme";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { retry: 1, refetchOnWindowFocus: false, staleTime: 5_000 },
    mutations: { retry: false },
  },
});

interface BoundaryState {
  error: Error | null;
}

class AppErrorBoundary extends Component<{ children: ReactNode }, BoundaryState> {
  state: BoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): BoundaryState {
    return { error };
  }

  componentDidCatch(_error: Error, _info: ErrorInfo) {
    // Rendering a bounded error is sufficient here; diagnostics remain in the browser console.
  }

  render() {
    if (this.state.error) {
      return (
        <Alert m="xl" color="red" icon={<AlertTriangle aria-hidden="true" />} title="The interface stopped">
          {this.state.error.message}
        </Alert>
      );
    }
    return this.props.children;
  }
}

export function AppProviders({ children }: { children: ReactNode }) {
  return (
    <MantineProvider theme={benchmarkTheme} defaultColorScheme="light">
      <AppErrorBoundary>
        <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
      </AppErrorBoundary>
    </MantineProvider>
  );
}
