import * as path from 'node:path';
import type { FileService } from '../../../io/index.js';
import type {
  AgentConfig,
  SettingsFile,
  PluginsDirectory,
  InstalledPluginsFile,
  KnownMarketplacesFile,
  InstallCountsCacheFile,
  PluginCacheEntry,
  PluginManifest,
  McpConfigFile,
  MarketplaceManifest,
  StatsigDirectory,
  StatsigCachedEvaluations,
  StatsigFailedLogs,
  StatsigLastModifiedTime,
  StatsigSessionId,
  StatsigStableId,
  IdeDirectory,
  IdeLockFile,
  ShellSnapshotsDirectory,
  ShellSnapshotFile,
  CacheDirectory,
  MyClosedIssuesFile,
  StatusLineCommandFile,
  TeamDirectory,
  TeamConfig,
  InboxMessage,
  PluginBlocklistFile,
  McpNeedsAuthCache,
} from '../../../types/index.js';

export interface ConfigParserOptions {
  allShellSnapshots?: boolean;
}

export interface ConfigParser {
  parseConfig(rootDir: string, options?: ConfigParserOptions): AgentConfig;
  empty(): AgentConfig;
}

export class ConfigParserImpl implements ConfigParser {
  constructor(private fileService: FileService) {}

  parseConfig(rootDir: string, options?: ConfigParserOptions): AgentConfig {
    return {
      settings: this.parseSettings(rootDir),
      settingsLocal: this.parseSettingsLocal(rootDir),
      plugins: this.parsePlugins(rootDir),
      statsig: this.parseStatsig(rootDir),
      ide: this.parseIde(rootDir),
      shellSnapshots: this.parseShellSnapshots(rootDir, options?.allShellSnapshots ?? false),
      cache: this.parseCache(rootDir),
      statusLineCommand: this.parseStatusLineCommand(rootDir),
      teams: this.parseTeams(rootDir),
      mcpNeedsAuth: this.parseMcpNeedsAuth(rootDir),
    };
  }

  empty(): AgentConfig {
    return {
      settings: { permissions: { allow: [] } },
      settingsLocal: null,
      plugins: {
        installedPlugins: { version: 2, plugins: {} },
        knownMarketplaces: {},
        installCountsCache: { version: 1, fetchedAt: '', counts: [] },
        cache: [],
        marketplaces: [],
        blocklist: null,
      },
      statsig: {},
      ide: { lockFiles: [] },
      shellSnapshots: { snapshots: [] },
      cache: {},
      statusLineCommand: null,
      teams: [],
      mcpNeedsAuth: null,
    };
  }

  private parseMcpNeedsAuth(rootDir: string): McpNeedsAuthCache | null {
    try {
      return this.fileService.readJsonSync<McpNeedsAuthCache>(path.join(rootDir, 'mcp-needs-auth-cache.json'));
    } catch {
      return null;
    }
  }

  private parseSettingsLocal(rootDir: string): SettingsFile | null {
    // Same schema as settings.json; overrides it per working directory.
    // Returns null (not an empty settings object) when absent so consumers
    // can tell "no local overrides" from "empty local file".
    try {
      return this.fileService.readJsonSync<SettingsFile>(path.join(rootDir, 'settings.local.json'));
    } catch {
      return null;
    }
  }

  private parseSettings(rootDir: string): SettingsFile {
    return this.readJsonSafe<SettingsFile>(path.join(rootDir, 'settings.json'), { permissions: { allow: [] } });
  }

  private parsePlugins(rootDir: string): PluginsDirectory {
    const pluginsDir = path.join(rootDir, 'plugins');

    const installedPlugins = this.readJsonSafe<InstalledPluginsFile>(path.join(pluginsDir, 'installed_plugins.json'), {
      version: 2,
      plugins: {},
    });
    const knownMarketplaces = this.readJsonSafe<KnownMarketplacesFile>(
      path.join(pluginsDir, 'known_marketplaces.json'),
      {},
    );
    const installCountsCache = this.readJsonSafe<InstallCountsCacheFile>(
      path.join(pluginsDir, 'install-counts-cache.json'),
      { version: 1, fetchedAt: '', counts: [] },
    );
    const cache = this.parsePluginCache(pluginsDir);
    const marketplaces = this.parseMarketplaces(pluginsDir);
    const blocklist = this.fileService.readJsonSync<PluginBlocklistFile>(path.join(pluginsDir, 'blocklist.json'));

    return { installedPlugins, knownMarketplaces, installCountsCache, cache, marketplaces, blocklist };
  }

  private parsePluginCache(pluginsDir: string): PluginCacheEntry[] {
    const entries: PluginCacheEntry[] = [];
    const cacheDir = path.join(pluginsDir, 'cache');

    try {
      const marketplacePaths = this.fileService.scanDirectorySync(cacheDir, { includeDirectories: true });

      for (const marketplacePath of marketplacePaths) {
        try {
          const marketplace = path.basename(marketplacePath);
          const pluginPaths = this.fileService.scanDirectorySync(marketplacePath, { includeDirectories: true });

          for (const pluginPath of pluginPaths) {
            try {
              const plugin = path.basename(pluginPath);
              const versionPaths = this.fileService.scanDirectorySync(pluginPath, { includeDirectories: true });

              for (const versionPath of versionPaths) {
                try {
                  const version = path.basename(versionPath);
                  const entry: PluginCacheEntry = { marketplace, plugin, version };

                  const manifest = this.fileService.readJsonSync<PluginManifest>(
                    path.join(versionPath, '.claude-plugin', 'plugin.json'),
                  );
                  if (manifest) entry.manifest = manifest;

                  const mcpConfig = this.fileService.readJsonSync<McpConfigFile>(path.join(versionPath, '.mcp.json'));
                  if (mcpConfig) entry.mcpConfig = mcpConfig;

                  try {
                    const orphanedContent = this.fileService.readFileSync(path.join(versionPath, '.orphaned_at'));
                    const ts = parseInt(orphanedContent.trim(), 10);
                    if (!isNaN(ts)) entry.orphanedAt = ts;
                  } catch {
                    /* optional */
                  }

                  entries.push(entry);
                } catch {
                  /* skip bad version dir */
                }
              }
            } catch {
              /* skip bad plugin dir */
            }
          }
        } catch {
          /* skip bad marketplace dir */
        }
      }
    } catch {
      // cache dir doesn't exist
    }

    return entries;
  }

  private parseMarketplaces(pluginsDir: string): MarketplaceManifest[] {
    const manifests: MarketplaceManifest[] = [];
    const marketplacesDir = path.join(pluginsDir, 'marketplaces');

    try {
      const dirPaths = this.fileService.scanDirectorySync(marketplacesDir, { includeDirectories: true });

      for (const dirPath of dirPaths) {
        const manifest = this.fileService.readJsonSync<MarketplaceManifest>(
          path.join(dirPath, '.claude-plugin', 'marketplace.json'),
        );
        if (manifest) manifests.push(manifest);
      }
    } catch {
      // marketplaces dir doesn't exist
    }

    return manifests;
  }

  private parseStatsig(rootDir: string): StatsigDirectory {
    const statsigDir = path.join(rootDir, 'statsig');
    const result: StatsigDirectory = {};

    try {
      const filePaths = this.fileService.scanDirectorySync(statsigDir);

      for (const filePath of filePaths) {
        const fileName = path.basename(filePath);

        try {
          if (fileName.startsWith('statsig.cached.evaluations.')) {
            result.cachedEvaluations = this.fileService.readJsonSync<StatsigCachedEvaluations>(filePath) ?? undefined;
          } else if (fileName.startsWith('statsig.failed_logs.')) {
            result.failedLogs = this.fileService.readJsonSync<StatsigFailedLogs>(filePath) ?? undefined;
          } else if (fileName === 'statsig.last_modified_time.evaluations') {
            result.lastModifiedTime = this.fileService.readJsonSync<StatsigLastModifiedTime>(filePath) ?? undefined;
          } else if (fileName.startsWith('statsig.session_id.')) {
            result.sessionId = this.fileService.readJsonSync<StatsigSessionId>(filePath) ?? undefined;
          } else if (fileName.startsWith('statsig.stable_id.')) {
            result.stableId = this.fileService.readJsonSync<StatsigStableId>(filePath) ?? undefined;
          }
        } catch {
          /* skip bad statsig file */
        }
      }
    } catch {
      // statsig dir doesn't exist
    }

    return result;
  }

  private parseIde(rootDir: string): IdeDirectory {
    try {
      const ideDir = path.join(rootDir, 'ide');
      const filePaths = this.fileService.scanDirectorySync(ideDir, { pattern: '*.lock' });

      const lockFiles: IdeLockFile[] = [];
      for (const filePath of filePaths) {
        const lockFile = this.fileService.readJsonSync<IdeLockFile>(filePath);
        if (lockFile) lockFiles.push(lockFile);
      }

      return { lockFiles };
    } catch {
      return { lockFiles: [] };
    }
  }

  private parseShellSnapshots(rootDir: string, all: boolean): ShellSnapshotsDirectory {
    try {
      const snapshotsDir = path.join(rootDir, 'shell-snapshots');
      const filePaths = this.fileService.scanDirectorySync(snapshotsDir, { pattern: 'snapshot-*.sh' });

      if (!all) {
        const latest = this.findLatestSnapshot(filePaths);
        if (!latest) return { snapshots: [] };
        return { snapshots: [latest] };
      }

      const snapshots: ShellSnapshotFile[] = [];
      for (const filePath of filePaths) {
        const snapshot = this.parseSnapshotFile(filePath);
        if (snapshot) snapshots.push(snapshot);
      }

      return { snapshots };
    } catch {
      return { snapshots: [] };
    }
  }

  private findLatestSnapshot(filePaths: string[]): ShellSnapshotFile | null {
    let latestPath: string | null = null;
    let latestTimestamp = -1;

    for (const filePath of filePaths) {
      const fileName = path.basename(filePath);
      const match = fileName.match(/^snapshot-\w+-(\d+)-\w+\.sh$/);
      if (!match) continue;
      const ts = parseInt(match[1], 10);
      if (ts > latestTimestamp) {
        latestTimestamp = ts;
        latestPath = filePath;
      }
    }

    return latestPath ? this.parseSnapshotFile(latestPath) : null;
  }

  private parseSnapshotFile(filePath: string): ShellSnapshotFile | null {
    try {
      const fileName = path.basename(filePath);
      const match = fileName.match(/^snapshot-(\w+)-(\d+)-(\w+)\.sh$/);
      if (!match) return null;

      const content = this.fileService.readFileSync(filePath);
      const stats = this.fileService.getStats(filePath);

      return {
        shell: match[1],
        timestamp: parseInt(match[2], 10),
        hash: match[3],
        fileName,
        content,
        size: stats?.size ?? 0,
      };
    } catch {
      return null;
    }
  }

  private parseTeams(rootDir: string): TeamDirectory[] {
    const teams: TeamDirectory[] = [];
    const teamsDir = path.join(rootDir, 'teams');

    try {
      const entryPaths = this.fileService.scanDirectorySync(teamsDir, { includeDirectories: true });

      for (const teamPath of entryPaths) {
        // Filters out stray files like .DS_Store; a directory is a team
        // even when config.json is missing or corrupt (observed in the wild).
        if (!this.fileService.getStats(teamPath)?.isDirectory) continue;

        let config: TeamConfig | null = null;
        try {
          config = this.fileService.readJsonSync<TeamConfig>(path.join(teamPath, 'config.json'));
        } catch {
          /* corrupt config.json — surface the team with config: null */
        }

        teams.push({
          teamId: path.basename(teamPath),
          config,
          inboxes: this.parseTeamInboxes(teamPath),
        });
      }
    } catch {
      // teams dir doesn't exist
    }

    return teams.sort((a, b) => a.teamId.localeCompare(b.teamId));
  }

  private parseTeamInboxes(teamPath: string): Record<string, InboxMessage[]> {
    const inboxes: Record<string, InboxMessage[]> = {};

    try {
      const inboxPaths = this.fileService.scanDirectorySync(path.join(teamPath, 'inboxes'), { pattern: '*.json' });

      for (const inboxPath of inboxPaths) {
        try {
          const messages = this.fileService.readJsonSync<InboxMessage[]>(inboxPath);
          if (Array.isArray(messages)) {
            inboxes[path.basename(inboxPath, '.json')] = messages;
          }
        } catch {
          /* skip bad inbox file */
        }
      }
    } catch {
      // inboxes dir doesn't exist
    }

    return inboxes;
  }

  private parseCache(rootDir: string): CacheDirectory {
    const result: CacheDirectory = {};

    try {
      const changelogPath = path.join(rootDir, 'cache', 'changelog.md');
      const content = this.fileService.readFileSync(changelogPath);
      const stats = this.fileService.getStats(changelogPath);
      result.changelog = { content, size: stats?.size ?? 0 };
    } catch {
      // no changelog
    }

    try {
      const issuesPath = path.join(rootDir, 'cache', 'my-closed-issues.json');
      const issues = this.fileService.readJsonSync<unknown>(issuesPath);
      if (Array.isArray(issues)) result.myClosedIssues = issues as MyClosedIssuesFile;
    } catch {
      // no issue cache (or an incomplete cache write)
    }

    return result;
  }

  private parseStatusLineCommand(rootDir: string): StatusLineCommandFile | null {
    try {
      const filePath = path.join(rootDir, 'statusline-command.sh');
      const content = this.fileService.readFileSync(filePath);
      const stats = this.fileService.getStats(filePath);
      return { content, size: stats?.size ?? 0 };
    } catch {
      return null;
    }
  }

  private readJsonSafe<T>(filePath: string, fallback: T): T {
    try {
      return this.fileService.readJsonSync<T>(filePath) ?? fallback;
    } catch {
      return fallback;
    }
  }
}

export function createConfigParser(fileService: FileService): ConfigParser {
  return new ConfigParserImpl(fileService);
}
