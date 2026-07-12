import { BrowserRouter, Route, Routes } from "react-router";
import { AppProviders } from "@/AppProviders";
import { BenchmarkAppShell } from "@/components/BenchmarkAppShell";
import { ComparePage } from "@/pages/ComparePage";
import { FamilyPage } from "@/pages/FamilyPage";
import { OverviewPage } from "@/pages/OverviewPage";
import { ReportPage } from "@/pages/ReportPage";
import { RunPage } from "@/pages/RunPage";

export function BenchmarkRoutes() {
  return (
    <Routes>
      <Route path="/benchmark" element={<BenchmarkAppShell />}>
        <Route index element={<OverviewPage />} />
        <Route path="command" element={<FamilyPage familyRouteId="command" />} />
        <Route path="files" element={<FamilyPage familyRouteId="files" />} />
        <Route path="workspace" element={<FamilyPage familyRouteId="workspace" />} />
        <Route path="layerstack" element={<FamilyPage familyRouteId="layerstack" />} />
        <Route path="runs/:runId" element={<RunPage />} />
        <Route path="reports/:runId" element={<ReportPage />} />
        <Route path="compare" element={<ComparePage />} />
      </Route>
    </Routes>
  );
}

export default function App() {
  return (
    <AppProviders>
      <BrowserRouter>
        <BenchmarkRoutes />
      </BrowserRouter>
    </AppProviders>
  );
}
