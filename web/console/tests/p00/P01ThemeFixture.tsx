import {
  Badge,
  Button,
  Group,
  MantineProvider,
  Paper,
  Stack,
  Text,
  TextInput,
  Title,
} from "@mantine/core";
import { useReducedMotion } from "@mantine/hooks";
import { createRoot } from "react-dom/client";
import { ephemeralosTheme } from "@/theme";
import "@mantine/core/styles.css";
import styles from "./p01-theme-fixture.module.css";

function ThemeSpecimen() {
  const reducedMotion = useReducedMotion();

  return (
    <main className={styles.specimen}>
      <header className={styles.header}>
        <img
          alt=""
          className={styles.logo}
          data-testid="brand-logo"
          height="36"
          src="/assets/images/logo.png"
          width="36"
        />
        <div>
          <Title order={1}>EphemeralOS operator theme</Title>
          <Text c="dimmed" size="sm">
            P01 fixture · light only · compact operational density
          </Text>
        </div>
      </header>

      <section aria-label="Theme tokens" className={styles.grid}>
        <Paper className={styles.panel} p="md" shadow="xs" withBorder>
          <Stack gap="sm">
            <Text fw={650}>Typography and controls</Text>
            <Text size="sm">Calm sans copy keeps an operator surface readable.</Text>
            <Text ff="monospace" size="xs">
              sbx_01H9C · /workspace/service · 14:32:08Z
            </Text>
            <TextInput error="Required for destructive operation" label="Command label" placeholder="e.g. deploy" />
            <Group gap="sm" wrap="wrap">
              <Button data-testid="primary-action">Primary action</Button>
              <Button aria-pressed="true" variant="light">
                Selected
              </Button>
              <Button disabled variant="default">
                Disabled
              </Button>
              <Button loading>Loading</Button>
              <Button color="danger">Destructive</Button>
            </Group>
          </Stack>
        </Paper>

        <Paper className={styles.panel} p="md" shadow="xs" withBorder>
          <Stack gap="sm">
            <Text fw={650}>Lifecycle and trust states</Text>
            <Group gap="sm" wrap="wrap">
              <Badge color="success" variant="light">
                Ready
              </Badge>
              <Badge color="eyeBlue" variant="light">
                Running
              </Badge>
              <Badge color="warning" variant="light">
                Stale
              </Badge>
              <Badge color="danger" variant="light">
                Error
              </Badge>
              <Badge color="warm" variant="light">
                Idle
              </Badge>
            </Group>
            <Text c="dimmed" size="sm">
              State never relies on color alone; each status keeps a text label.
            </Text>
            <Text data-testid="motion-state" ff="monospace" size="xs">
              motion: {reducedMotion ? "reduced" : "full"}
            </Text>
          </Stack>
        </Paper>
      </section>
    </main>
  );
}

export function P01ThemeFixture() {
  return (
    <MantineProvider forceColorScheme="light" theme={ephemeralosTheme}>
      <ThemeSpecimen />
    </MantineProvider>
  );
}

createRoot(document.getElementById("root")!).render(<P01ThemeFixture />);
