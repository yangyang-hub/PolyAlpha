import { Routes, Route } from "react-router-dom";
import Layout from "./components/Layout";
import Dashboard from "./pages/Dashboard";
import StrategyConfig from "./pages/StrategyConfig";
import RiskConfig from "./pages/RiskConfig";
import MarketConfig from "./pages/MarketConfig";
import MonitorConfig from "./pages/MonitorConfig";

export default function App() {
  return (
    <Layout>
      <Routes>
        <Route path="/" element={<Dashboard />} />
        <Route path="/strategy" element={<StrategyConfig />} />
        <Route path="/risk" element={<RiskConfig />} />
        <Route path="/market" element={<MarketConfig />} />
        <Route path="/monitor" element={<MonitorConfig />} />
      </Routes>
    </Layout>
  );
}
