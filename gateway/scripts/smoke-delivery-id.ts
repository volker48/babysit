import { randomUUID } from "node:crypto";

export function smokeDeliveryId(): string {
  return `gateway-smoke-${randomUUID()}`;
}
