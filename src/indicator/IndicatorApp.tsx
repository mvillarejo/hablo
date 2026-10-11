import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isIndicatorPhase, type IndicatorPhase } from "../lib/indicatorBars";
import { Indicator } from "./Indicator";

export function IndicatorApp() {
  const [phase, setPhase] = useState<IndicatorPhase>("hidden");
  const [meter, setMeter] = useState({ level: 0, tick: 0 });

  useEffect(() => {
    const apply = (value: unknown) => {
      if (!isIndicatorPhase(value)) return;
      setPhase(value);
      if (value !== "listening") setMeter((m) => ({ ...m, level: 0 }));
    };
    // The window may load after the first state event; ask once.
    void invoke<IndicatorPhase>("get_indicator_phase").then(apply, () => {});
    const subs = [
      listen<IndicatorPhase>("indicator-state", (e) => apply(e.payload)),
      listen<number>("mic-level", (e) =>
        setMeter((m) => ({ level: Number(e.payload), tick: m.tick + 1 })),
      ),
    ];
    return () => subs.forEach((p) => void p.then((off) => off()));
  }, []);

  return <Indicator phase={phase} level={meter.level} tick={meter.tick} />;
}
