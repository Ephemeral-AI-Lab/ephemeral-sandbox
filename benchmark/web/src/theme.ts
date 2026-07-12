import { createTheme, type MantineColorsTuple } from "@mantine/core";

const instrument: MantineColorsTuple = [
  "#edf3f8",
  "#d7e3ec",
  "#adc5d7",
  "#80a5c1",
  "#5b8aae",
  "#41799f",
  "#326b92",
  "#245879",
  "#173f59",
  "#17324d",
];

export const benchmarkTheme = createTheme({
  primaryColor: "instrument",
  colors: { instrument },
  fontFamily: '"Fira Sans", system-ui, sans-serif',
  fontFamilyMonospace: '"Fira Code", ui-monospace, monospace',
  headings: { fontFamily: '"Fira Sans", system-ui, sans-serif', fontWeight: "600" },
  breakpoints: { xs: "30em", sm: "48em", md: "64em", lg: "75em", xl: "100em" },
  defaultRadius: "sm",
  focusRing: "always",
});
