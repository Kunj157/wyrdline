import { MotionConfig } from "motion/react";

import { Footer, Nav } from "./components/Chrome";
import { Hero } from "./components/Hero";
import { SaidVsDid } from "./components/SaidVsDid";

export function App() {
  return (
    <MotionConfig reducedMotion="user">
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:z-50 focus:rounded-sm focus:bg-surface-1 focus:px-3 focus:py-2"
      >
        Skip to content
      </a>
      <Nav />
      <main id="main">
        <Hero />
        <SaidVsDid />
      </main>
      <Footer />
    </MotionConfig>
  );
}
