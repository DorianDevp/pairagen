import { SchedulerEvent } from "./types";
import { applyAll, emptyState } from "./store";
import { roomUsage, busiestRoom } from "./report";

const events: SchedulerEvent[] = [
  { kind: "book", id: "a", room: "R1", start: 540, end: 600 },
  { kind: "book", id: "b", room: "R1", start: 600, end: 660 },
  { kind: "reschedule", id: "a", start: 555, end: 615 },
  { kind: "cancel", id: "b" },
];

export function run(): void {
  const state = applyAll(emptyState(), events);
  const usage = roomUsage(state);
  const winner = busiestRoom(usage);
  console.log(winner, usage);
}
