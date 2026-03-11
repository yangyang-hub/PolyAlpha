import { Routes, Route } from "react-router-dom";
import Layout from "./components/Layout";
import Dashboard from "./pages/Dashboard";
import WeatherStrategy from "./pages/WeatherStrategy";
import SmartMoney from "./pages/SmartMoney";
import LRRewards from "./pages/LRRewards";
import CryptoMarkets from "./pages/CryptoMarkets";
import Configuration from "./pages/Configuration";

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={<Dashboard />} />
        <Route path="weather" element={<WeatherStrategy />} />
        <Route path="smart-money" element={<SmartMoney />} />
        <Route path="lr" element={<LRRewards />} />
        <Route path="crypto" element={<CryptoMarkets />} />
        <Route path="config" element={<Configuration />} />
      </Route>
    </Routes>
  );
}
