export function smokeChildEnvironment(
  parent: NodeJS.ProcessEnv,
  additions: NodeJS.ProcessEnv = {},
): NodeJS.ProcessEnv {
  const {
    WATCHER_TOKEN: _parentWatcherToken,
    WEBHOOK_SECRET: _parentWebhookSecret,
    ...parentEnvironment
  } = parent;
  const {
    WATCHER_TOKEN: _addedWatcherToken,
    WEBHOOK_SECRET: _addedWebhookSecret,
    ...additionalEnvironment
  } = additions;
  return { ...parentEnvironment, ...additionalEnvironment };
}
