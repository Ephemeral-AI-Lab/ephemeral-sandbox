import {
  ActionIcon,
  Badge,
  Button,
  Input,
  Modal,
  Paper,
  createTheme,
  type MantineColorsTuple,
} from "@mantine/core";

export const CONSOLE_BREAKPOINTS = {
  xs: "30em",
  sm: "48em",
  md: "64em",
  lg: "80em",
  xl: "90em",
} as const;

export const CONSOLE_Z_INDEX = {
  app: 100,
  modal: 200,
  popover: 300,
  overlay: 400,
  max: 9999,
} as const;

const warm: MantineColorsTuple = [
  "#fefdfc",
  "#faf5ef",
  "#f2e9de",
  "#e7dccc",
  "#dacbbb",
  "#c6ad97",
  "#a98c75",
  "#896c58",
  "#60493b",
  "#3d2f28",
];

const neutral: MantineColorsTuple = [
  "#fdfcfb",
  "#f7f4f0",
  "#ece7e1",
  "#ded6ce",
  "#c9beb3",
  "#9b8e84",
  "#6d625b",
  "#524941",
  "#3a332e",
  "#271f1c",
];

const eyeBlue: MantineColorsTuple = [
  "#f0f7fb",
  "#e0eef5",
  "#c7dfea",
  "#a1c3d7",
  "#87aec4",
  "#68889e",
  "#527890",
  "#3e6077",
  "#2e4b60",
  "#203746",
];

const success: MantineColorsTuple = [
  "#effbf3",
  "#dcf5e5",
  "#b9e9cb",
  "#8bd6a7",
  "#5fbe83",
  "#3d9c64",
  "#267a50",
  "#1e623f",
  "#174d32",
  "#103b26",
];

const warning: MantineColorsTuple = [
  "#fff8e8",
  "#ffedc2",
  "#ffdc91",
  "#f7c35f",
  "#dfa330",
  "#c78210",
  "#a35c00",
  "#844900",
  "#683900",
  "#4d2a00",
];

const danger: MantineColorsTuple = [
  "#fff1f1",
  "#ffdfdf",
  "#ffc4c4",
  "#f99b9b",
  "#e96f6f",
  "#d24d4d",
  "#b83535",
  "#962a2a",
  "#782121",
  "#5c1818",
];

export const ephemeralosTheme = createTheme({
  primaryColor: "eyeBlue",
  primaryShade: 7,
  colors: {
    warm,
    neutral,
    eyeBlue,
    success,
    warning,
    danger,
    blue: eyeBlue,
    gray: neutral,
    red: danger,
    teal: success,
  },
  white: "#fffdfb",
  black: "#271f1c",
  fontFamily: 'Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif',
  fontFamilyMonospace:
    'ui-monospace, "JetBrains Mono", "SFMono-Regular", Menlo, Consolas, monospace',
  headings: {
    fontFamily: 'Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif',
    fontWeight: "650",
  },
  fontSizes: {
    xs: "0.75rem",
    sm: "0.8125rem",
    md: "0.875rem",
    lg: "1rem",
    xl: "1.125rem",
  },
  lineHeights: {
    xs: "1.3",
    sm: "1.35",
    md: "1.45",
    lg: "1.45",
    xl: "1.4",
  },
  spacing: {
    xs: "0.25rem",
    sm: "0.5rem",
    md: "0.75rem",
    lg: "1rem",
    xl: "1.5rem",
  },
  radius: {
    xs: "0.25rem",
    sm: "0.375rem",
    md: "0.5rem",
    lg: "0.625rem",
    xl: "0.75rem",
  },
  defaultRadius: "sm",
  shadows: {
    xs: "0 1px 2px rgb(39 31 28 / 0.06)",
    sm: "0 2px 6px rgb(39 31 28 / 0.08)",
    md: "0 8px 20px rgb(39 31 28 / 0.1)",
  },
  breakpoints: CONSOLE_BREAKPOINTS,
  focusRing: "auto",
  cursorType: "pointer",
  respectReducedMotion: true,
  other: {
    canvas: warm[0],
    surface: "#fffaf5",
    border: warm[4],
    text: "#271f1c",
    textSecondary: "#5e5148",
    focus: eyeBlue[7],
    running: eyeBlue[7],
    idle: warm[7],
    zIndex: CONSOLE_Z_INDEX,
  },
  components: {
    ActionIcon: ActionIcon.extend({ defaultProps: { radius: "sm", size: "sm" } }),
    Badge: Badge.extend({ defaultProps: { radius: "sm", size: "sm" } }),
    Button: Button.extend({ defaultProps: { radius: "sm", size: "compact-sm" } }),
    Input: Input.extend({ defaultProps: { radius: "sm" } }),
    Modal: Modal.extend({
      defaultProps: { overlayProps: { backgroundOpacity: 0.16, blur: 0 }, radius: "md" },
    }),
    Paper: Paper.extend({ defaultProps: { radius: "sm" } }),
  },
});
