export interface WakeEvent {
  deliveryId: string;
  kind: string;
  repository: {
    id: string;
    fullName: string;
  };
  changeNumber?: number;
  headRevision?: string;
  receivedAt: number;
}

export interface WakeRegistration {
  changeNumber: number;
  headRevision: string;
}

export type WakeRoute = "change" | "revision" | "repository";

export function selectWakeRoute(
  event: WakeEvent,
  registrations: readonly WakeRegistration[],
): WakeRoute {
  if (
    event.changeNumber !== undefined &&
    registrations.some((registration) => registration.changeNumber === event.changeNumber)
  ) {
    return "change";
  }
  if (
    event.headRevision !== undefined &&
    registrations.some((registration) => registration.headRevision === event.headRevision)
  ) {
    return "revision";
  }
  return "repository";
}

export function matchesWake(
  event: WakeEvent,
  registration: WakeRegistration,
  route: WakeRoute,
): boolean {
  if (route === "change") return registration.changeNumber === event.changeNumber;
  if (route === "revision") return registration.headRevision === event.headRevision;
  return true;
}
