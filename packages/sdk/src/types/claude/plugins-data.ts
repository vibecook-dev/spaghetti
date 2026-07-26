/**
 * TypeScript interfaces for ~/.claude/plugins/
 */

export interface InstalledPluginEntry {
  scope: 'user';
  installPath: string;
  version: string;
  installedAt: string;
  lastUpdated: string;
  gitCommitSha?: string;
}

export interface InstalledPluginsFile {
  version: number;
  plugins: Record<string, InstalledPluginEntry[]>;
}

export type MarketplaceSource =
  | {
      source: 'github';
      /** `owner/name`, not a URL. */
      repo: string;
    }
  | {
      /**
       * Any git remote, including SSH — used for marketplaces outside
       * GitHub, or GitHub ones added by clone URL rather than `owner/name`.
       */
      source: 'git';
      url: string;
    }
  | {
      source: 'directory';
      path: string;
    };

export interface KnownMarketplaceEntry {
  source: MarketplaceSource;
  installLocation: string;
  lastUpdated: string;
  autoUpdate?: boolean;
}

export type KnownMarketplacesFile = Record<string, KnownMarketplaceEntry>;

export interface PluginInstallCount {
  plugin: string;
  unique_installs: number;
}

export interface InstallCountsCacheFile {
  version: number;
  fetchedAt: string;
  counts: PluginInstallCount[];
}

export interface PluginAuthor {
  name: string;
  email?: string;
  url?: string;
}

export interface PluginManifest {
  name: string;
  version?: string;
  description: string;
  author?: PluginAuthor;
  repository?: string;
  homepage?: string;
  license?: string;
  keywords?: string[];
  // A path (string) or a list of paths (string[]) in real manifests.
  skills?: string | string[];
  commands?: string | string[];
  agents?: string | string[];
}

export type ExtensionToLanguageMap = Record<string, string>;

export interface LspServerConfig {
  command: string;
  args?: string[];
  extensionToLanguage: ExtensionToLanguageMap;
}

export interface MarketplacePluginEntry {
  name: string;
  description: string;
  source: string;
  category?: string;
  version?: string;
  author?: PluginAuthor;
  strict?: boolean;
  lspServers?: Record<string, LspServerConfig>;
  homepage?: string;
  tags?: string[];
}

export interface MarketplaceOwner {
  name: string;
  email?: string;
}

export interface MarketplaceManifest {
  $schema?: string;
  name: string;
  version?: string;
  description?: string;
  owner: MarketplaceOwner;
  plugins: MarketplacePluginEntry[];
}

export interface McpServerDefinition {
  command: string;
  args: string[];
  env?: Record<string, string>;
}

export type McpConfigFile = Record<string, McpServerDefinition>;

export interface OrphanedMarker {
  orphanedAt: number;
}

export interface PluginCacheEntry {
  marketplace: string;
  plugin: string;
  version: string;
  manifest?: PluginManifest;
  mcpConfig?: McpConfigFile;
  orphanedAt?: number;
}

export interface PluginsDirectory {
  installedPlugins: InstalledPluginsFile;
  knownMarketplaces: KnownMarketplacesFile;
  installCountsCache: InstallCountsCacheFile;
  cache: PluginCacheEntry[];
  marketplaces: MarketplaceManifest[];
  blocklist: PluginBlocklistFile | null;
}

/** `~/.claude/plugins/blocklist.json` — plugins CC has blocked. */
export interface PluginBlocklistFile {
  fetchedAt: string;
  plugins: PluginBlocklistEntry[];
}

export interface PluginBlocklistEntry {
  /** `{plugin}@{marketplace}` */
  plugin: string;
  added_at: string;
  reason?: string;
  text?: string;
}
