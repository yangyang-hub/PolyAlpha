import { ReactNode } from "react";
import { NavLink } from "react-router-dom";

const navItems = [
  { to: "/", label: "概览" },
  { to: "/strategy", label: "策略" },
  { to: "/risk", label: "风控" },
  { to: "/market", label: "市场" },
  { to: "/monitor", label: "监控" },
];

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <div className="drawer lg:drawer-open">
      <input id="nav-drawer" type="checkbox" className="drawer-toggle" />
      <div className="drawer-content flex flex-col">
        {/* Navbar */}
        <div className="navbar bg-base-200 lg:hidden">
          <div className="flex-none">
            <label htmlFor="nav-drawer" className="btn btn-square btn-ghost">
              <svg xmlns="http://www.w3.org/2000/svg" className="h-5 w-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 6h16M4 12h16M4 18h16" />
              </svg>
            </label>
          </div>
          <div className="flex-1">
            <span className="text-xl font-bold px-2">PolyAlpha</span>
          </div>
        </div>
        {/* Page content */}
        <main className="p-4 lg:p-6 flex-1">{children}</main>
      </div>
      {/* Sidebar */}
      <div className="drawer-side">
        <label htmlFor="nav-drawer" className="drawer-overlay" />
        <aside className="menu bg-base-200 w-64 min-h-full p-4">
          <div className="text-xl font-bold mb-6 px-2">PolyAlpha</div>
          <ul>
            {navItems.map((item) => (
              <li key={item.to}>
                <NavLink
                  to={item.to}
                  end={item.to === "/"}
                  className={({ isActive }) =>
                    isActive ? "active font-semibold" : ""
                  }
                >
                  {item.label}
                </NavLink>
              </li>
            ))}
          </ul>
        </aside>
      </div>
    </div>
  );
}
