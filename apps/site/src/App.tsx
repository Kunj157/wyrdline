import { MotionConfig } from "motion/react";

import { Footer, Nav } from "./components/Chrome";
import { Demo } from "./components/Demo";
import { Features } from "./components/Features";
import { Hero } from "./components/Hero";
import { GetStarted, Roadmap } from "./components/Roadmap";
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
        <Features />
        <Demo />
        <Roadmap />
        <GetStarted />
      </main>
      <Footer />
    </MotionConfig>
  );
}
