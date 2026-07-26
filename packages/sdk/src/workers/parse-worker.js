// src/workers/parse-worker.ts
import { parentPort } from "node:worker_threads";

// src/io/file-service.ts
import { EventEmitter as EventEmitter2 } from "events";
import {
  readFileSync,
  writeFileSync,
  appendFileSync,
  existsSync,
  statSync as statSync2,
  mkdirSync,
  readdirSync,
  openSync as openSync2,
  readSync as readSync2,
  closeSync as closeSync2,
  watch as fsWatch
} from "fs";
import { readFile, writeFile, appendFile, unlink } from "fs/promises";
import { dirname as dirname3, join as join3 } from "path";

// ../../node_modules/.pnpm/chokidar@4.0.3/node_modules/chokidar/esm/index.js
import { stat as statcb } from "fs";
import { stat as stat3, readdir as readdir2 } from "fs/promises";
import { EventEmitter } from "events";
import * as sysPath2 from "path";

// ../../node_modules/.pnpm/readdirp@4.1.2/node_modules/readdirp/esm/index.js
import { stat, lstat, readdir, realpath } from "node:fs/promises";
import { Readable } from "node:stream";
import { resolve as presolve, relative as prelative, join as pjoin, sep as psep } from "node:path";
var EntryTypes = {
  FILE_TYPE: "files",
  DIR_TYPE: "directories",
  FILE_DIR_TYPE: "files_directories",
  EVERYTHING_TYPE: "all"
};
var defaultOptions = {
  root: ".",
  fileFilter: (_entryInfo) => true,
  directoryFilter: (_entryInfo) => true,
  type: EntryTypes.FILE_TYPE,
  lstat: false,
  depth: 2147483648,
  alwaysStat: false,
  highWaterMark: 4096
};
Object.freeze(defaultOptions);
var RECURSIVE_ERROR_CODE = "READDIRP_RECURSIVE_ERROR";
var NORMAL_FLOW_ERRORS = /* @__PURE__ */ new Set(["ENOENT", "EPERM", "EACCES", "ELOOP", RECURSIVE_ERROR_CODE]);
var ALL_TYPES = [
  EntryTypes.DIR_TYPE,
  EntryTypes.EVERYTHING_TYPE,
  EntryTypes.FILE_DIR_TYPE,
  EntryTypes.FILE_TYPE
];
var DIR_TYPES = /* @__PURE__ */ new Set([
  EntryTypes.DIR_TYPE,
  EntryTypes.EVERYTHING_TYPE,
  EntryTypes.FILE_DIR_TYPE
]);
var FILE_TYPES = /* @__PURE__ */ new Set([
  EntryTypes.EVERYTHING_TYPE,
  EntryTypes.FILE_DIR_TYPE,
  EntryTypes.FILE_TYPE
]);
var isNormalFlowError = (error) => NORMAL_FLOW_ERRORS.has(error.code);
var wantBigintFsStats = process.platform === "win32";
var emptyFn = (_entryInfo) => true;
var normalizeFilter = (filter) => {
  if (filter === void 0)
    return emptyFn;
  if (typeof filter === "function")
    return filter;
  if (typeof filter === "string") {
    const fl = filter.trim();
    return (entry) => entry.basename === fl;
  }
  if (Array.isArray(filter)) {
    const trItems = filter.map((item) => item.trim());
    return (entry) => trItems.some((f) => entry.basename === f);
  }
  return emptyFn;
};
var ReaddirpStream = class extends Readable {
  constructor(options = {}) {
    super({
      objectMode: true,
      autoDestroy: true,
      highWaterMark: options.highWaterMark
    });
    const opts = { ...defaultOptions, ...options };
    const { root, type } = opts;
    this._fileFilter = normalizeFilter(opts.fileFilter);
    this._directoryFilter = normalizeFilter(opts.directoryFilter);
    const statMethod = opts.lstat ? lstat : stat;
    if (wantBigintFsStats) {
      this._stat = (path2) => statMethod(path2, { bigint: true });
    } else {
      this._stat = statMethod;
    }
    this._maxDepth = opts.depth ?? defaultOptions.depth;
    this._wantsDir = type ? DIR_TYPES.has(type) : false;
    this._wantsFile = type ? FILE_TYPES.has(type) : false;
    this._wantsEverything = type === EntryTypes.EVERYTHING_TYPE;
    this._root = presolve(root);
    this._isDirent = !opts.alwaysStat;
    this._statsProp = this._isDirent ? "dirent" : "stats";
    this._rdOptions = { encoding: "utf8", withFileTypes: this._isDirent };
    this.parents = [this._exploreDir(root, 1)];
    this.reading = false;
    this.parent = void 0;
  }
  async _read(batch) {
    if (this.reading)
      return;
    this.reading = true;
    try {
      while (!this.destroyed && batch > 0) {
        const par = this.parent;
        const fil = par && par.files;
        if (fil && fil.length > 0) {
          const { path: path2, depth } = par;
          const slice = fil.splice(0, batch).map((dirent) => this._formatEntry(dirent, path2));
          const awaited = await Promise.all(slice);
          for (const entry of awaited) {
            if (!entry)
              continue;
            if (this.destroyed)
              return;
            const entryType = await this._getEntryType(entry);
            if (entryType === "directory" && this._directoryFilter(entry)) {
              if (depth <= this._maxDepth) {
                this.parents.push(this._exploreDir(entry.fullPath, depth + 1));
              }
              if (this._wantsDir) {
                this.push(entry);
                batch--;
              }
            } else if ((entryType === "file" || this._includeAsFile(entry)) && this._fileFilter(entry)) {
              if (this._wantsFile) {
                this.push(entry);
                batch--;
              }
            }
          }
        } else {
          const parent = this.parents.pop();
          if (!parent) {
            this.push(null);
            break;
          }
          this.parent = await parent;
          if (this.destroyed)
            return;
        }
      }
    } catch (error) {
      this.destroy(error);
    } finally {
      this.reading = false;
    }
  }
  async _exploreDir(path2, depth) {
    let files;
    try {
      files = await readdir(path2, this._rdOptions);
    } catch (error) {
      this._onError(error);
    }
    return { files, depth, path: path2 };
  }
  async _formatEntry(dirent, path2) {
    let entry;
    const basename4 = this._isDirent ? dirent.name : dirent;
    try {
      const fullPath = presolve(pjoin(path2, basename4));
      entry = { path: prelative(this._root, fullPath), fullPath, basename: basename4 };
      entry[this._statsProp] = this._isDirent ? dirent : await this._stat(fullPath);
    } catch (err) {
      this._onError(err);
      return;
    }
    return entry;
  }
  _onError(err) {
    if (isNormalFlowError(err) && !this.destroyed) {
      this.emit("warn", err);
    } else {
      this.destroy(err);
    }
  }
  async _getEntryType(entry) {
    if (!entry && this._statsProp in entry) {
      return "";
    }
    const stats = entry[this._statsProp];
    if (stats.isFile())
      return "file";
    if (stats.isDirectory())
      return "directory";
    if (stats && stats.isSymbolicLink()) {
      const full = entry.fullPath;
      try {
        const entryRealPath = await realpath(full);
        const entryRealPathStats = await lstat(entryRealPath);
        if (entryRealPathStats.isFile()) {
          return "file";
        }
        if (entryRealPathStats.isDirectory()) {
          const len = entryRealPath.length;
          if (full.startsWith(entryRealPath) && full.substr(len, 1) === psep) {
            const recursiveError = new Error(`Circular symlink detected: "${full}" points to "${entryRealPath}"`);
            recursiveError.code = RECURSIVE_ERROR_CODE;
            return this._onError(recursiveError);
          }
          return "directory";
        }
      } catch (error) {
        this._onError(error);
        return "";
      }
    }
  }
  _includeAsFile(entry) {
    const stats = entry && entry[this._statsProp];
    return stats && this._wantsEverything && !stats.isDirectory();
  }
};
function readdirp(root, options = {}) {
  let type = options.entryType || options.type;
  if (type === "both")
    type = EntryTypes.FILE_DIR_TYPE;
  if (type)
    options.type = type;
  if (!root) {
    throw new Error("readdirp: root argument is required. Usage: readdirp(root, options)");
  } else if (typeof root !== "string") {
    throw new TypeError("readdirp: root argument must be a string. Usage: readdirp(root, options)");
  } else if (type && !ALL_TYPES.includes(type)) {
    throw new Error(`readdirp: Invalid type passed. Use one of ${ALL_TYPES.join(", ")}`);
  }
  options.root = root;
  return new ReaddirpStream(options);
}

// ../../node_modules/.pnpm/chokidar@4.0.3/node_modules/chokidar/esm/handler.js
import { watchFile, unwatchFile, watch as fs_watch } from "fs";
import { open, stat as stat2, lstat as lstat2, realpath as fsrealpath } from "fs/promises";
import * as sysPath from "path";
import { type as osType } from "os";
var STR_DATA = "data";
var STR_END = "end";
var STR_CLOSE = "close";
var EMPTY_FN = () => {
};
var pl = process.platform;
var isWindows = pl === "win32";
var isMacos = pl === "darwin";
var isLinux = pl === "linux";
var isFreeBSD = pl === "freebsd";
var isIBMi = osType() === "OS400";
var EVENTS = {
  ALL: "all",
  READY: "ready",
  ADD: "add",
  CHANGE: "change",
  ADD_DIR: "addDir",
  UNLINK: "unlink",
  UNLINK_DIR: "unlinkDir",
  RAW: "raw",
  ERROR: "error"
};
var EV = EVENTS;
var THROTTLE_MODE_WATCH = "watch";
var statMethods = { lstat: lstat2, stat: stat2 };
var KEY_LISTENERS = "listeners";
var KEY_ERR = "errHandlers";
var KEY_RAW = "rawEmitters";
var HANDLER_KEYS = [KEY_LISTENERS, KEY_ERR, KEY_RAW];
var binaryExtensions = /* @__PURE__ */ new Set([
  "3dm",
  "3ds",
  "3g2",
  "3gp",
  "7z",
  "a",
  "aac",
  "adp",
  "afdesign",
  "afphoto",
  "afpub",
  "ai",
  "aif",
  "aiff",
  "alz",
  "ape",
  "apk",
  "appimage",
  "ar",
  "arj",
  "asf",
  "au",
  "avi",
  "bak",
  "baml",
  "bh",
  "bin",
  "bk",
  "bmp",
  "btif",
  "bz2",
  "bzip2",
  "cab",
  "caf",
  "cgm",
  "class",
  "cmx",
  "cpio",
  "cr2",
  "cur",
  "dat",
  "dcm",
  "deb",
  "dex",
  "djvu",
  "dll",
  "dmg",
  "dng",
  "doc",
  "docm",
  "docx",
  "dot",
  "dotm",
  "dra",
  "DS_Store",
  "dsk",
  "dts",
  "dtshd",
  "dvb",
  "dwg",
  "dxf",
  "ecelp4800",
  "ecelp7470",
  "ecelp9600",
  "egg",
  "eol",
  "eot",
  "epub",
  "exe",
  "f4v",
  "fbs",
  "fh",
  "fla",
  "flac",
  "flatpak",
  "fli",
  "flv",
  "fpx",
  "fst",
  "fvt",
  "g3",
  "gh",
  "gif",
  "graffle",
  "gz",
  "gzip",
  "h261",
  "h263",
  "h264",
  "icns",
  "ico",
  "ief",
  "img",
  "ipa",
  "iso",
  "jar",
  "jpeg",
  "jpg",
  "jpgv",
  "jpm",
  "jxr",
  "key",
  "ktx",
  "lha",
  "lib",
  "lvp",
  "lz",
  "lzh",
  "lzma",
  "lzo",
  "m3u",
  "m4a",
  "m4v",
  "mar",
  "mdi",
  "mht",
  "mid",
  "midi",
  "mj2",
  "mka",
  "mkv",
  "mmr",
  "mng",
  "mobi",
  "mov",
  "movie",
  "mp3",
  "mp4",
  "mp4a",
  "mpeg",
  "mpg",
  "mpga",
  "mxu",
  "nef",
  "npx",
  "numbers",
  "nupkg",
  "o",
  "odp",
  "ods",
  "odt",
  "oga",
  "ogg",
  "ogv",
  "otf",
  "ott",
  "pages",
  "pbm",
  "pcx",
  "pdb",
  "pdf",
  "pea",
  "pgm",
  "pic",
  "png",
  "pnm",
  "pot",
  "potm",
  "potx",
  "ppa",
  "ppam",
  "ppm",
  "pps",
  "ppsm",
  "ppsx",
  "ppt",
  "pptm",
  "pptx",
  "psd",
  "pya",
  "pyc",
  "pyo",
  "pyv",
  "qt",
  "rar",
  "ras",
  "raw",
  "resources",
  "rgb",
  "rip",
  "rlc",
  "rmf",
  "rmvb",
  "rpm",
  "rtf",
  "rz",
  "s3m",
  "s7z",
  "scpt",
  "sgi",
  "shar",
  "snap",
  "sil",
  "sketch",
  "slk",
  "smv",
  "snk",
  "so",
  "stl",
  "suo",
  "sub",
  "swf",
  "tar",
  "tbz",
  "tbz2",
  "tga",
  "tgz",
  "thmx",
  "tif",
  "tiff",
  "tlz",
  "ttc",
  "ttf",
  "txz",
  "udf",
  "uvh",
  "uvi",
  "uvm",
  "uvp",
  "uvs",
  "uvu",
  "viv",
  "vob",
  "war",
  "wav",
  "wax",
  "wbmp",
  "wdp",
  "weba",
  "webm",
  "webp",
  "whl",
  "wim",
  "wm",
  "wma",
  "wmv",
  "wmx",
  "woff",
  "woff2",
  "wrm",
  "wvx",
  "xbm",
  "xif",
  "xla",
  "xlam",
  "xls",
  "xlsb",
  "xlsm",
  "xlsx",
  "xlt",
  "xltm",
  "xltx",
  "xm",
  "xmind",
  "xpi",
  "xpm",
  "xwd",
  "xz",
  "z",
  "zip",
  "zipx"
]);
var isBinaryPath = (filePath) => binaryExtensions.has(sysPath.extname(filePath).slice(1).toLowerCase());
var foreach = (val, fn) => {
  if (val instanceof Set) {
    val.forEach(fn);
  } else {
    fn(val);
  }
};
var addAndConvert = (main, prop, item) => {
  let container = main[prop];
  if (!(container instanceof Set)) {
    main[prop] = container = /* @__PURE__ */ new Set([container]);
  }
  container.add(item);
};
var clearItem = (cont) => (key) => {
  const set = cont[key];
  if (set instanceof Set) {
    set.clear();
  } else {
    delete cont[key];
  }
};
var delFromSet = (main, prop, item) => {
  const container = main[prop];
  if (container instanceof Set) {
    container.delete(item);
  } else if (container === item) {
    delete main[prop];
  }
};
var isEmptySet = (val) => val instanceof Set ? val.size === 0 : !val;
var FsWatchInstances = /* @__PURE__ */ new Map();
function createFsWatchInstance(path2, options, listener, errHandler, emitRaw) {
  const handleEvent = (rawEvent, evPath) => {
    listener(path2);
    emitRaw(rawEvent, evPath, { watchedPath: path2 });
    if (evPath && path2 !== evPath) {
      fsWatchBroadcast(sysPath.resolve(path2, evPath), KEY_LISTENERS, sysPath.join(path2, evPath));
    }
  };
  try {
    return fs_watch(path2, {
      persistent: options.persistent
    }, handleEvent);
  } catch (error) {
    errHandler(error);
    return void 0;
  }
}
var fsWatchBroadcast = (fullPath, listenerType, val1, val2, val3) => {
  const cont = FsWatchInstances.get(fullPath);
  if (!cont)
    return;
  foreach(cont[listenerType], (listener) => {
    listener(val1, val2, val3);
  });
};
var setFsWatchListener = (path2, fullPath, options, handlers) => {
  const { listener, errHandler, rawEmitter } = handlers;
  let cont = FsWatchInstances.get(fullPath);
  let watcher;
  if (!options.persistent) {
    watcher = createFsWatchInstance(path2, options, listener, errHandler, rawEmitter);
    if (!watcher)
      return;
    return watcher.close.bind(watcher);
  }
  if (cont) {
    addAndConvert(cont, KEY_LISTENERS, listener);
    addAndConvert(cont, KEY_ERR, errHandler);
    addAndConvert(cont, KEY_RAW, rawEmitter);
  } else {
    watcher = createFsWatchInstance(
      path2,
      options,
      fsWatchBroadcast.bind(null, fullPath, KEY_LISTENERS),
      errHandler,
      // no need to use broadcast here
      fsWatchBroadcast.bind(null, fullPath, KEY_RAW)
    );
    if (!watcher)
      return;
    watcher.on(EV.ERROR, async (error) => {
      const broadcastErr = fsWatchBroadcast.bind(null, fullPath, KEY_ERR);
      if (cont)
        cont.watcherUnusable = true;
      if (isWindows && error.code === "EPERM") {
        try {
          const fd = await open(path2, "r");
          await fd.close();
          broadcastErr(error);
        } catch (err) {
        }
      } else {
        broadcastErr(error);
      }
    });
    cont = {
      listeners: listener,
      errHandlers: errHandler,
      rawEmitters: rawEmitter,
      watcher
    };
    FsWatchInstances.set(fullPath, cont);
  }
  return () => {
    delFromSet(cont, KEY_LISTENERS, listener);
    delFromSet(cont, KEY_ERR, errHandler);
    delFromSet(cont, KEY_RAW, rawEmitter);
    if (isEmptySet(cont.listeners)) {
      cont.watcher.close();
      FsWatchInstances.delete(fullPath);
      HANDLER_KEYS.forEach(clearItem(cont));
      cont.watcher = void 0;
      Object.freeze(cont);
    }
  };
};
var FsWatchFileInstances = /* @__PURE__ */ new Map();
var setFsWatchFileListener = (path2, fullPath, options, handlers) => {
  const { listener, rawEmitter } = handlers;
  let cont = FsWatchFileInstances.get(fullPath);
  const copts = cont && cont.options;
  if (copts && (copts.persistent < options.persistent || copts.interval > options.interval)) {
    unwatchFile(fullPath);
    cont = void 0;
  }
  if (cont) {
    addAndConvert(cont, KEY_LISTENERS, listener);
    addAndConvert(cont, KEY_RAW, rawEmitter);
  } else {
    cont = {
      listeners: listener,
      rawEmitters: rawEmitter,
      options,
      watcher: watchFile(fullPath, options, (curr, prev) => {
        foreach(cont.rawEmitters, (rawEmitter2) => {
          rawEmitter2(EV.CHANGE, fullPath, { curr, prev });
        });
        const currmtime = curr.mtimeMs;
        if (curr.size !== prev.size || currmtime > prev.mtimeMs || currmtime === 0) {
          foreach(cont.listeners, (listener2) => listener2(path2, curr));
        }
      })
    };
    FsWatchFileInstances.set(fullPath, cont);
  }
  return () => {
    delFromSet(cont, KEY_LISTENERS, listener);
    delFromSet(cont, KEY_RAW, rawEmitter);
    if (isEmptySet(cont.listeners)) {
      FsWatchFileInstances.delete(fullPath);
      unwatchFile(fullPath);
      cont.options = cont.watcher = void 0;
      Object.freeze(cont);
    }
  };
};
var NodeFsHandler = class {
  constructor(fsW) {
    this.fsw = fsW;
    this._boundHandleError = (error) => fsW._handleError(error);
  }
  /**
   * Watch file for changes with fs_watchFile or fs_watch.
   * @param path to file or dir
   * @param listener on fs change
   * @returns closer for the watcher instance
   */
  _watchWithNodeFs(path2, listener) {
    const opts = this.fsw.options;
    const directory = sysPath.dirname(path2);
    const basename4 = sysPath.basename(path2);
    const parent = this.fsw._getWatchedDir(directory);
    parent.add(basename4);
    const absolutePath = sysPath.resolve(path2);
    const options = {
      persistent: opts.persistent
    };
    if (!listener)
      listener = EMPTY_FN;
    let closer;
    if (opts.usePolling) {
      const enableBin = opts.interval !== opts.binaryInterval;
      options.interval = enableBin && isBinaryPath(basename4) ? opts.binaryInterval : opts.interval;
      closer = setFsWatchFileListener(path2, absolutePath, options, {
        listener,
        rawEmitter: this.fsw._emitRaw
      });
    } else {
      closer = setFsWatchListener(path2, absolutePath, options, {
        listener,
        errHandler: this._boundHandleError,
        rawEmitter: this.fsw._emitRaw
      });
    }
    return closer;
  }
  /**
   * Watch a file and emit add event if warranted.
   * @returns closer for the watcher instance
   */
  _handleFile(file, stats, initialAdd) {
    if (this.fsw.closed) {
      return;
    }
    const dirname4 = sysPath.dirname(file);
    const basename4 = sysPath.basename(file);
    const parent = this.fsw._getWatchedDir(dirname4);
    let prevStats = stats;
    if (parent.has(basename4))
      return;
    const listener = async (path2, newStats) => {
      if (!this.fsw._throttle(THROTTLE_MODE_WATCH, file, 5))
        return;
      if (!newStats || newStats.mtimeMs === 0) {
        try {
          const newStats2 = await stat2(file);
          if (this.fsw.closed)
            return;
          const at = newStats2.atimeMs;
          const mt = newStats2.mtimeMs;
          if (!at || at <= mt || mt !== prevStats.mtimeMs) {
            this.fsw._emit(EV.CHANGE, file, newStats2);
          }
          if ((isMacos || isLinux || isFreeBSD) && prevStats.ino !== newStats2.ino) {
            this.fsw._closeFile(path2);
            prevStats = newStats2;
            const closer2 = this._watchWithNodeFs(file, listener);
            if (closer2)
              this.fsw._addPathCloser(path2, closer2);
          } else {
            prevStats = newStats2;
          }
        } catch (error) {
          this.fsw._remove(dirname4, basename4);
        }
      } else if (parent.has(basename4)) {
        const at = newStats.atimeMs;
        const mt = newStats.mtimeMs;
        if (!at || at <= mt || mt !== prevStats.mtimeMs) {
          this.fsw._emit(EV.CHANGE, file, newStats);
        }
        prevStats = newStats;
      }
    };
    const closer = this._watchWithNodeFs(file, listener);
    if (!(initialAdd && this.fsw.options.ignoreInitial) && this.fsw._isntIgnored(file)) {
      if (!this.fsw._throttle(EV.ADD, file, 0))
        return;
      this.fsw._emit(EV.ADD, file, stats);
    }
    return closer;
  }
  /**
   * Handle symlinks encountered while reading a dir.
   * @param entry returned by readdirp
   * @param directory path of dir being read
   * @param path of this item
   * @param item basename of this item
   * @returns true if no more processing is needed for this entry.
   */
  async _handleSymlink(entry, directory, path2, item) {
    if (this.fsw.closed) {
      return;
    }
    const full = entry.fullPath;
    const dir = this.fsw._getWatchedDir(directory);
    if (!this.fsw.options.followSymlinks) {
      this.fsw._incrReadyCount();
      let linkPath;
      try {
        linkPath = await fsrealpath(path2);
      } catch (e) {
        this.fsw._emitReady();
        return true;
      }
      if (this.fsw.closed)
        return;
      if (dir.has(item)) {
        if (this.fsw._symlinkPaths.get(full) !== linkPath) {
          this.fsw._symlinkPaths.set(full, linkPath);
          this.fsw._emit(EV.CHANGE, path2, entry.stats);
        }
      } else {
        dir.add(item);
        this.fsw._symlinkPaths.set(full, linkPath);
        this.fsw._emit(EV.ADD, path2, entry.stats);
      }
      this.fsw._emitReady();
      return true;
    }
    if (this.fsw._symlinkPaths.has(full)) {
      return true;
    }
    this.fsw._symlinkPaths.set(full, true);
  }
  _handleRead(directory, initialAdd, wh, target, dir, depth, throttler) {
    directory = sysPath.join(directory, "");
    throttler = this.fsw._throttle("readdir", directory, 1e3);
    if (!throttler)
      return;
    const previous = this.fsw._getWatchedDir(wh.path);
    const current = /* @__PURE__ */ new Set();
    let stream = this.fsw._readdirp(directory, {
      fileFilter: (entry) => wh.filterPath(entry),
      directoryFilter: (entry) => wh.filterDir(entry)
    });
    if (!stream)
      return;
    stream.on(STR_DATA, async (entry) => {
      if (this.fsw.closed) {
        stream = void 0;
        return;
      }
      const item = entry.path;
      let path2 = sysPath.join(directory, item);
      current.add(item);
      if (entry.stats.isSymbolicLink() && await this._handleSymlink(entry, directory, path2, item)) {
        return;
      }
      if (this.fsw.closed) {
        stream = void 0;
        return;
      }
      if (item === target || !target && !previous.has(item)) {
        this.fsw._incrReadyCount();
        path2 = sysPath.join(dir, sysPath.relative(dir, path2));
        this._addToNodeFs(path2, initialAdd, wh, depth + 1);
      }
    }).on(EV.ERROR, this._boundHandleError);
    return new Promise((resolve3, reject) => {
      if (!stream)
        return reject();
      stream.once(STR_END, () => {
        if (this.fsw.closed) {
          stream = void 0;
          return;
        }
        const wasThrottled = throttler ? throttler.clear() : false;
        resolve3(void 0);
        previous.getChildren().filter((item) => {
          return item !== directory && !current.has(item);
        }).forEach((item) => {
          this.fsw._remove(directory, item);
        });
        stream = void 0;
        if (wasThrottled)
          this._handleRead(directory, false, wh, target, dir, depth, throttler);
      });
    });
  }
  /**
   * Read directory to add / remove files from `@watched` list and re-read it on change.
   * @param dir fs path
   * @param stats
   * @param initialAdd
   * @param depth relative to user-supplied path
   * @param target child path targeted for watch
   * @param wh Common watch helpers for this path
   * @param realpath
   * @returns closer for the watcher instance.
   */
  async _handleDir(dir, stats, initialAdd, depth, target, wh, realpath2) {
    const parentDir = this.fsw._getWatchedDir(sysPath.dirname(dir));
    const tracked = parentDir.has(sysPath.basename(dir));
    if (!(initialAdd && this.fsw.options.ignoreInitial) && !target && !tracked) {
      this.fsw._emit(EV.ADD_DIR, dir, stats);
    }
    parentDir.add(sysPath.basename(dir));
    this.fsw._getWatchedDir(dir);
    let throttler;
    let closer;
    const oDepth = this.fsw.options.depth;
    if ((oDepth == null || depth <= oDepth) && !this.fsw._symlinkPaths.has(realpath2)) {
      if (!target) {
        await this._handleRead(dir, initialAdd, wh, target, dir, depth, throttler);
        if (this.fsw.closed)
          return;
      }
      closer = this._watchWithNodeFs(dir, (dirPath, stats2) => {
        if (stats2 && stats2.mtimeMs === 0)
          return;
        this._handleRead(dirPath, false, wh, target, dir, depth, throttler);
      });
    }
    return closer;
  }
  /**
   * Handle added file, directory, or glob pattern.
   * Delegates call to _handleFile / _handleDir after checks.
   * @param path to file or ir
   * @param initialAdd was the file added at watch instantiation?
   * @param priorWh depth relative to user-supplied path
   * @param depth Child path actually targeted for watch
   * @param target Child path actually targeted for watch
   */
  async _addToNodeFs(path2, initialAdd, priorWh, depth, target) {
    const ready = this.fsw._emitReady;
    if (this.fsw._isIgnored(path2) || this.fsw.closed) {
      ready();
      return false;
    }
    const wh = this.fsw._getWatchHelpers(path2);
    if (priorWh) {
      wh.filterPath = (entry) => priorWh.filterPath(entry);
      wh.filterDir = (entry) => priorWh.filterDir(entry);
    }
    try {
      const stats = await statMethods[wh.statMethod](wh.watchPath);
      if (this.fsw.closed)
        return;
      if (this.fsw._isIgnored(wh.watchPath, stats)) {
        ready();
        return false;
      }
      const follow = this.fsw.options.followSymlinks;
      let closer;
      if (stats.isDirectory()) {
        const absPath = sysPath.resolve(path2);
        const targetPath = follow ? await fsrealpath(path2) : path2;
        if (this.fsw.closed)
          return;
        closer = await this._handleDir(wh.watchPath, stats, initialAdd, depth, target, wh, targetPath);
        if (this.fsw.closed)
          return;
        if (absPath !== targetPath && targetPath !== void 0) {
          this.fsw._symlinkPaths.set(absPath, targetPath);
        }
      } else if (stats.isSymbolicLink()) {
        const targetPath = follow ? await fsrealpath(path2) : path2;
        if (this.fsw.closed)
          return;
        const parent = sysPath.dirname(wh.watchPath);
        this.fsw._getWatchedDir(parent).add(wh.watchPath);
        this.fsw._emit(EV.ADD, wh.watchPath, stats);
        closer = await this._handleDir(parent, stats, initialAdd, depth, path2, wh, targetPath);
        if (this.fsw.closed)
          return;
        if (targetPath !== void 0) {
          this.fsw._symlinkPaths.set(sysPath.resolve(path2), targetPath);
        }
      } else {
        closer = this._handleFile(wh.watchPath, stats, initialAdd);
      }
      ready();
      if (closer)
        this.fsw._addPathCloser(path2, closer);
      return false;
    } catch (error) {
      if (this.fsw._handleError(error)) {
        ready();
        return path2;
      }
    }
  }
};

// ../../node_modules/.pnpm/chokidar@4.0.3/node_modules/chokidar/esm/index.js
var SLASH = "/";
var SLASH_SLASH = "//";
var ONE_DOT = ".";
var TWO_DOTS = "..";
var STRING_TYPE = "string";
var BACK_SLASH_RE = /\\/g;
var DOUBLE_SLASH_RE = /\/\//;
var DOT_RE = /\..*\.(sw[px])$|~$|\.subl.*\.tmp/;
var REPLACER_RE = /^\.[/\\]/;
function arrify(item) {
  return Array.isArray(item) ? item : [item];
}
var isMatcherObject = (matcher) => typeof matcher === "object" && matcher !== null && !(matcher instanceof RegExp);
function createPattern(matcher) {
  if (typeof matcher === "function")
    return matcher;
  if (typeof matcher === "string")
    return (string) => matcher === string;
  if (matcher instanceof RegExp)
    return (string) => matcher.test(string);
  if (typeof matcher === "object" && matcher !== null) {
    return (string) => {
      if (matcher.path === string)
        return true;
      if (matcher.recursive) {
        const relative3 = sysPath2.relative(matcher.path, string);
        if (!relative3) {
          return false;
        }
        return !relative3.startsWith("..") && !sysPath2.isAbsolute(relative3);
      }
      return false;
    };
  }
  return () => false;
}
function normalizePath(path2) {
  if (typeof path2 !== "string")
    throw new Error("string expected");
  path2 = sysPath2.normalize(path2);
  path2 = path2.replace(/\\/g, "/");
  let prepend = false;
  if (path2.startsWith("//"))
    prepend = true;
  const DOUBLE_SLASH_RE2 = /\/\//;
  while (path2.match(DOUBLE_SLASH_RE2))
    path2 = path2.replace(DOUBLE_SLASH_RE2, "/");
  if (prepend)
    path2 = "/" + path2;
  return path2;
}
function matchPatterns(patterns, testString, stats) {
  const path2 = normalizePath(testString);
  for (let index = 0; index < patterns.length; index++) {
    const pattern = patterns[index];
    if (pattern(path2, stats)) {
      return true;
    }
  }
  return false;
}
function anymatch(matchers, testString) {
  if (matchers == null) {
    throw new TypeError("anymatch: specify first argument");
  }
  const matchersArray = arrify(matchers);
  const patterns = matchersArray.map((matcher) => createPattern(matcher));
  if (testString == null) {
    return (testString2, stats) => {
      return matchPatterns(patterns, testString2, stats);
    };
  }
  return matchPatterns(patterns, testString);
}
var unifyPaths = (paths_) => {
  const paths = arrify(paths_).flat();
  if (!paths.every((p) => typeof p === STRING_TYPE)) {
    throw new TypeError(`Non-string provided as watch path: ${paths}`);
  }
  return paths.map(normalizePathToUnix);
};
var toUnix = (string) => {
  let str = string.replace(BACK_SLASH_RE, SLASH);
  let prepend = false;
  if (str.startsWith(SLASH_SLASH)) {
    prepend = true;
  }
  while (str.match(DOUBLE_SLASH_RE)) {
    str = str.replace(DOUBLE_SLASH_RE, SLASH);
  }
  if (prepend) {
    str = SLASH + str;
  }
  return str;
};
var normalizePathToUnix = (path2) => toUnix(sysPath2.normalize(toUnix(path2)));
var normalizeIgnored = (cwd = "") => (path2) => {
  if (typeof path2 === "string") {
    return normalizePathToUnix(sysPath2.isAbsolute(path2) ? path2 : sysPath2.join(cwd, path2));
  } else {
    return path2;
  }
};
var getAbsolutePath = (path2, cwd) => {
  if (sysPath2.isAbsolute(path2)) {
    return path2;
  }
  return sysPath2.join(cwd, path2);
};
var EMPTY_SET = Object.freeze(/* @__PURE__ */ new Set());
var DirEntry = class {
  constructor(dir, removeWatcher) {
    this.path = dir;
    this._removeWatcher = removeWatcher;
    this.items = /* @__PURE__ */ new Set();
  }
  add(item) {
    const { items } = this;
    if (!items)
      return;
    if (item !== ONE_DOT && item !== TWO_DOTS)
      items.add(item);
  }
  async remove(item) {
    const { items } = this;
    if (!items)
      return;
    items.delete(item);
    if (items.size > 0)
      return;
    const dir = this.path;
    try {
      await readdir2(dir);
    } catch (err) {
      if (this._removeWatcher) {
        this._removeWatcher(sysPath2.dirname(dir), sysPath2.basename(dir));
      }
    }
  }
  has(item) {
    const { items } = this;
    if (!items)
      return;
    return items.has(item);
  }
  getChildren() {
    const { items } = this;
    if (!items)
      return [];
    return [...items.values()];
  }
  dispose() {
    this.items.clear();
    this.path = "";
    this._removeWatcher = EMPTY_FN;
    this.items = EMPTY_SET;
    Object.freeze(this);
  }
};
var STAT_METHOD_F = "stat";
var STAT_METHOD_L = "lstat";
var WatchHelper = class {
  constructor(path2, follow, fsw) {
    this.fsw = fsw;
    const watchPath = path2;
    this.path = path2 = path2.replace(REPLACER_RE, "");
    this.watchPath = watchPath;
    this.fullWatchPath = sysPath2.resolve(watchPath);
    this.dirParts = [];
    this.dirParts.forEach((parts) => {
      if (parts.length > 1)
        parts.pop();
    });
    this.followSymlinks = follow;
    this.statMethod = follow ? STAT_METHOD_F : STAT_METHOD_L;
  }
  entryPath(entry) {
    return sysPath2.join(this.watchPath, sysPath2.relative(this.watchPath, entry.fullPath));
  }
  filterPath(entry) {
    const { stats } = entry;
    if (stats && stats.isSymbolicLink())
      return this.filterDir(entry);
    const resolvedPath = this.entryPath(entry);
    return this.fsw._isntIgnored(resolvedPath, stats) && this.fsw._hasReadPermissions(stats);
  }
  filterDir(entry) {
    return this.fsw._isntIgnored(this.entryPath(entry), entry.stats);
  }
};
var FSWatcher = class extends EventEmitter {
  // Not indenting methods for history sake; for now.
  constructor(_opts = {}) {
    super();
    this.closed = false;
    this._closers = /* @__PURE__ */ new Map();
    this._ignoredPaths = /* @__PURE__ */ new Set();
    this._throttled = /* @__PURE__ */ new Map();
    this._streams = /* @__PURE__ */ new Set();
    this._symlinkPaths = /* @__PURE__ */ new Map();
    this._watched = /* @__PURE__ */ new Map();
    this._pendingWrites = /* @__PURE__ */ new Map();
    this._pendingUnlinks = /* @__PURE__ */ new Map();
    this._readyCount = 0;
    this._readyEmitted = false;
    const awf = _opts.awaitWriteFinish;
    const DEF_AWF = { stabilityThreshold: 2e3, pollInterval: 100 };
    const opts = {
      // Defaults
      persistent: true,
      ignoreInitial: false,
      ignorePermissionErrors: false,
      interval: 100,
      binaryInterval: 300,
      followSymlinks: true,
      usePolling: false,
      // useAsync: false,
      atomic: true,
      // NOTE: overwritten later (depends on usePolling)
      ..._opts,
      // Change format
      ignored: _opts.ignored ? arrify(_opts.ignored) : arrify([]),
      awaitWriteFinish: awf === true ? DEF_AWF : typeof awf === "object" ? { ...DEF_AWF, ...awf } : false
    };
    if (isIBMi)
      opts.usePolling = true;
    if (opts.atomic === void 0)
      opts.atomic = !opts.usePolling;
    const envPoll = process.env.CHOKIDAR_USEPOLLING;
    if (envPoll !== void 0) {
      const envLower = envPoll.toLowerCase();
      if (envLower === "false" || envLower === "0")
        opts.usePolling = false;
      else if (envLower === "true" || envLower === "1")
        opts.usePolling = true;
      else
        opts.usePolling = !!envLower;
    }
    const envInterval = process.env.CHOKIDAR_INTERVAL;
    if (envInterval)
      opts.interval = Number.parseInt(envInterval, 10);
    let readyCalls = 0;
    this._emitReady = () => {
      readyCalls++;
      if (readyCalls >= this._readyCount) {
        this._emitReady = EMPTY_FN;
        this._readyEmitted = true;
        process.nextTick(() => this.emit(EVENTS.READY));
      }
    };
    this._emitRaw = (...args) => this.emit(EVENTS.RAW, ...args);
    this._boundRemove = this._remove.bind(this);
    this.options = opts;
    this._nodeFsHandler = new NodeFsHandler(this);
    Object.freeze(opts);
  }
  _addIgnoredPath(matcher) {
    if (isMatcherObject(matcher)) {
      for (const ignored of this._ignoredPaths) {
        if (isMatcherObject(ignored) && ignored.path === matcher.path && ignored.recursive === matcher.recursive) {
          return;
        }
      }
    }
    this._ignoredPaths.add(matcher);
  }
  _removeIgnoredPath(matcher) {
    this._ignoredPaths.delete(matcher);
    if (typeof matcher === "string") {
      for (const ignored of this._ignoredPaths) {
        if (isMatcherObject(ignored) && ignored.path === matcher) {
          this._ignoredPaths.delete(ignored);
        }
      }
    }
  }
  // Public methods
  /**
   * Adds paths to be watched on an existing FSWatcher instance.
   * @param paths_ file or file list. Other arguments are unused
   */
  add(paths_, _origAdd, _internal) {
    const { cwd } = this.options;
    this.closed = false;
    this._closePromise = void 0;
    let paths = unifyPaths(paths_);
    if (cwd) {
      paths = paths.map((path2) => {
        const absPath = getAbsolutePath(path2, cwd);
        return absPath;
      });
    }
    paths.forEach((path2) => {
      this._removeIgnoredPath(path2);
    });
    this._userIgnored = void 0;
    if (!this._readyCount)
      this._readyCount = 0;
    this._readyCount += paths.length;
    Promise.all(paths.map(async (path2) => {
      const res = await this._nodeFsHandler._addToNodeFs(path2, !_internal, void 0, 0, _origAdd);
      if (res)
        this._emitReady();
      return res;
    })).then((results) => {
      if (this.closed)
        return;
      results.forEach((item) => {
        if (item)
          this.add(sysPath2.dirname(item), sysPath2.basename(_origAdd || item));
      });
    });
    return this;
  }
  /**
   * Close watchers or start ignoring events from specified paths.
   */
  unwatch(paths_) {
    if (this.closed)
      return this;
    const paths = unifyPaths(paths_);
    const { cwd } = this.options;
    paths.forEach((path2) => {
      if (!sysPath2.isAbsolute(path2) && !this._closers.has(path2)) {
        if (cwd)
          path2 = sysPath2.join(cwd, path2);
        path2 = sysPath2.resolve(path2);
      }
      this._closePath(path2);
      this._addIgnoredPath(path2);
      if (this._watched.has(path2)) {
        this._addIgnoredPath({
          path: path2,
          recursive: true
        });
      }
      this._userIgnored = void 0;
    });
    return this;
  }
  /**
   * Close watchers and remove all listeners from watched paths.
   */
  close() {
    if (this._closePromise) {
      return this._closePromise;
    }
    this.closed = true;
    this.removeAllListeners();
    const closers = [];
    this._closers.forEach((closerList) => closerList.forEach((closer) => {
      const promise = closer();
      if (promise instanceof Promise)
        closers.push(promise);
    }));
    this._streams.forEach((stream) => stream.destroy());
    this._userIgnored = void 0;
    this._readyCount = 0;
    this._readyEmitted = false;
    this._watched.forEach((dirent) => dirent.dispose());
    this._closers.clear();
    this._watched.clear();
    this._streams.clear();
    this._symlinkPaths.clear();
    this._throttled.clear();
    this._closePromise = closers.length ? Promise.all(closers).then(() => void 0) : Promise.resolve();
    return this._closePromise;
  }
  /**
   * Expose list of watched paths
   * @returns for chaining
   */
  getWatched() {
    const watchList = {};
    this._watched.forEach((entry, dir) => {
      const key = this.options.cwd ? sysPath2.relative(this.options.cwd, dir) : dir;
      const index = key || ONE_DOT;
      watchList[index] = entry.getChildren().sort();
    });
    return watchList;
  }
  emitWithAll(event, args) {
    this.emit(event, ...args);
    if (event !== EVENTS.ERROR)
      this.emit(EVENTS.ALL, event, ...args);
  }
  // Common helpers
  // --------------
  /**
   * Normalize and emit events.
   * Calling _emit DOES NOT MEAN emit() would be called!
   * @param event Type of event
   * @param path File or directory path
   * @param stats arguments to be passed with event
   * @returns the error if defined, otherwise the value of the FSWatcher instance's `closed` flag
   */
  async _emit(event, path2, stats) {
    if (this.closed)
      return;
    const opts = this.options;
    if (isWindows)
      path2 = sysPath2.normalize(path2);
    if (opts.cwd)
      path2 = sysPath2.relative(opts.cwd, path2);
    const args = [path2];
    if (stats != null)
      args.push(stats);
    const awf = opts.awaitWriteFinish;
    let pw;
    if (awf && (pw = this._pendingWrites.get(path2))) {
      pw.lastChange = /* @__PURE__ */ new Date();
      return this;
    }
    if (opts.atomic) {
      if (event === EVENTS.UNLINK) {
        this._pendingUnlinks.set(path2, [event, ...args]);
        setTimeout(() => {
          this._pendingUnlinks.forEach((entry, path3) => {
            this.emit(...entry);
            this.emit(EVENTS.ALL, ...entry);
            this._pendingUnlinks.delete(path3);
          });
        }, typeof opts.atomic === "number" ? opts.atomic : 100);
        return this;
      }
      if (event === EVENTS.ADD && this._pendingUnlinks.has(path2)) {
        event = EVENTS.CHANGE;
        this._pendingUnlinks.delete(path2);
      }
    }
    if (awf && (event === EVENTS.ADD || event === EVENTS.CHANGE) && this._readyEmitted) {
      const awfEmit = (err, stats2) => {
        if (err) {
          event = EVENTS.ERROR;
          args[0] = err;
          this.emitWithAll(event, args);
        } else if (stats2) {
          if (args.length > 1) {
            args[1] = stats2;
          } else {
            args.push(stats2);
          }
          this.emitWithAll(event, args);
        }
      };
      this._awaitWriteFinish(path2, awf.stabilityThreshold, event, awfEmit);
      return this;
    }
    if (event === EVENTS.CHANGE) {
      const isThrottled = !this._throttle(EVENTS.CHANGE, path2, 50);
      if (isThrottled)
        return this;
    }
    if (opts.alwaysStat && stats === void 0 && (event === EVENTS.ADD || event === EVENTS.ADD_DIR || event === EVENTS.CHANGE)) {
      const fullPath = opts.cwd ? sysPath2.join(opts.cwd, path2) : path2;
      let stats2;
      try {
        stats2 = await stat3(fullPath);
      } catch (err) {
      }
      if (!stats2 || this.closed)
        return;
      args.push(stats2);
    }
    this.emitWithAll(event, args);
    return this;
  }
  /**
   * Common handler for errors
   * @returns The error if defined, otherwise the value of the FSWatcher instance's `closed` flag
   */
  _handleError(error) {
    const code = error && error.code;
    if (error && code !== "ENOENT" && code !== "ENOTDIR" && (!this.options.ignorePermissionErrors || code !== "EPERM" && code !== "EACCES")) {
      this.emit(EVENTS.ERROR, error);
    }
    return error || this.closed;
  }
  /**
   * Helper utility for throttling
   * @param actionType type being throttled
   * @param path being acted upon
   * @param timeout duration of time to suppress duplicate actions
   * @returns tracking object or false if action should be suppressed
   */
  _throttle(actionType, path2, timeout) {
    if (!this._throttled.has(actionType)) {
      this._throttled.set(actionType, /* @__PURE__ */ new Map());
    }
    const action = this._throttled.get(actionType);
    if (!action)
      throw new Error("invalid throttle");
    const actionPath = action.get(path2);
    if (actionPath) {
      actionPath.count++;
      return false;
    }
    let timeoutObject;
    const clear = () => {
      const item = action.get(path2);
      const count = item ? item.count : 0;
      action.delete(path2);
      clearTimeout(timeoutObject);
      if (item)
        clearTimeout(item.timeoutObject);
      return count;
    };
    timeoutObject = setTimeout(clear, timeout);
    const thr = { timeoutObject, clear, count: 0 };
    action.set(path2, thr);
    return thr;
  }
  _incrReadyCount() {
    return this._readyCount++;
  }
  /**
   * Awaits write operation to finish.
   * Polls a newly created file for size variations. When files size does not change for 'threshold' milliseconds calls callback.
   * @param path being acted upon
   * @param threshold Time in milliseconds a file size must be fixed before acknowledging write OP is finished
   * @param event
   * @param awfEmit Callback to be called when ready for event to be emitted.
   */
  _awaitWriteFinish(path2, threshold, event, awfEmit) {
    const awf = this.options.awaitWriteFinish;
    if (typeof awf !== "object")
      return;
    const pollInterval = awf.pollInterval;
    let timeoutHandler;
    let fullPath = path2;
    if (this.options.cwd && !sysPath2.isAbsolute(path2)) {
      fullPath = sysPath2.join(this.options.cwd, path2);
    }
    const now = /* @__PURE__ */ new Date();
    const writes = this._pendingWrites;
    function awaitWriteFinishFn(prevStat) {
      statcb(fullPath, (err, curStat) => {
        if (err || !writes.has(path2)) {
          if (err && err.code !== "ENOENT")
            awfEmit(err);
          return;
        }
        const now2 = Number(/* @__PURE__ */ new Date());
        if (prevStat && curStat.size !== prevStat.size) {
          writes.get(path2).lastChange = now2;
        }
        const pw = writes.get(path2);
        const df = now2 - pw.lastChange;
        if (df >= threshold) {
          writes.delete(path2);
          awfEmit(void 0, curStat);
        } else {
          timeoutHandler = setTimeout(awaitWriteFinishFn, pollInterval, curStat);
        }
      });
    }
    if (!writes.has(path2)) {
      writes.set(path2, {
        lastChange: now,
        cancelWait: () => {
          writes.delete(path2);
          clearTimeout(timeoutHandler);
          return event;
        }
      });
      timeoutHandler = setTimeout(awaitWriteFinishFn, pollInterval);
    }
  }
  /**
   * Determines whether user has asked to ignore this path.
   */
  _isIgnored(path2, stats) {
    if (this.options.atomic && DOT_RE.test(path2))
      return true;
    if (!this._userIgnored) {
      const { cwd } = this.options;
      const ign = this.options.ignored;
      const ignored = (ign || []).map(normalizeIgnored(cwd));
      const ignoredPaths = [...this._ignoredPaths];
      const list = [...ignoredPaths.map(normalizeIgnored(cwd)), ...ignored];
      this._userIgnored = anymatch(list, void 0);
    }
    return this._userIgnored(path2, stats);
  }
  _isntIgnored(path2, stat4) {
    return !this._isIgnored(path2, stat4);
  }
  /**
   * Provides a set of common helpers and properties relating to symlink handling.
   * @param path file or directory pattern being watched
   */
  _getWatchHelpers(path2) {
    return new WatchHelper(path2, this.options.followSymlinks, this);
  }
  // Directory helpers
  // -----------------
  /**
   * Provides directory tracking objects
   * @param directory path of the directory
   */
  _getWatchedDir(directory) {
    const dir = sysPath2.resolve(directory);
    if (!this._watched.has(dir))
      this._watched.set(dir, new DirEntry(dir, this._boundRemove));
    return this._watched.get(dir);
  }
  // File helpers
  // ------------
  /**
   * Check for read permissions: https://stackoverflow.com/a/11781404/1358405
   */
  _hasReadPermissions(stats) {
    if (this.options.ignorePermissionErrors)
      return true;
    return Boolean(Number(stats.mode) & 256);
  }
  /**
   * Handles emitting unlink events for
   * files and directories, and via recursion, for
   * files and directories within directories that are unlinked
   * @param directory within which the following item is located
   * @param item      base path of item/directory
   */
  _remove(directory, item, isDirectory) {
    const path2 = sysPath2.join(directory, item);
    const fullPath = sysPath2.resolve(path2);
    isDirectory = isDirectory != null ? isDirectory : this._watched.has(path2) || this._watched.has(fullPath);
    if (!this._throttle("remove", path2, 100))
      return;
    if (!isDirectory && this._watched.size === 1) {
      this.add(directory, item, true);
    }
    const wp = this._getWatchedDir(path2);
    const nestedDirectoryChildren = wp.getChildren();
    nestedDirectoryChildren.forEach((nested) => this._remove(path2, nested));
    const parent = this._getWatchedDir(directory);
    const wasTracked = parent.has(item);
    parent.remove(item);
    if (this._symlinkPaths.has(fullPath)) {
      this._symlinkPaths.delete(fullPath);
    }
    let relPath = path2;
    if (this.options.cwd)
      relPath = sysPath2.relative(this.options.cwd, path2);
    if (this.options.awaitWriteFinish && this._pendingWrites.has(relPath)) {
      const event = this._pendingWrites.get(relPath).cancelWait();
      if (event === EVENTS.ADD)
        return;
    }
    this._watched.delete(path2);
    this._watched.delete(fullPath);
    const eventName = isDirectory ? EVENTS.UNLINK_DIR : EVENTS.UNLINK;
    if (wasTracked && !this._isIgnored(path2))
      this._emit(eventName, path2);
    this._closePath(path2);
  }
  /**
   * Closes all watchers for a path
   */
  _closePath(path2) {
    this._closeFile(path2);
    const dir = sysPath2.dirname(path2);
    this._getWatchedDir(dir).remove(sysPath2.basename(path2));
  }
  /**
   * Closes only file-specific watchers
   */
  _closeFile(path2) {
    const closers = this._closers.get(path2);
    if (!closers)
      return;
    closers.forEach((closer) => closer());
    this._closers.delete(path2);
  }
  _addPathCloser(path2, closer) {
    if (!closer)
      return;
    let list = this._closers.get(path2);
    if (!list) {
      list = [];
      this._closers.set(path2, list);
    }
    list.push(closer);
  }
  _readdirp(root, opts) {
    if (this.closed)
      return;
    const options = { type: EVENTS.ALL, alwaysStat: true, lstat: true, ...opts, depth: 0 };
    let stream = readdirp(root, options);
    this._streams.add(stream);
    stream.once(STR_CLOSE, () => {
      stream = void 0;
    });
    stream.once(STR_END, () => {
      if (stream) {
        this._streams.delete(stream);
        stream = void 0;
      }
    });
    return stream;
  }
};
function watch(paths, options = {}) {
  const watcher = new FSWatcher(options);
  watcher.add(paths);
  return watcher;
}
var esm_default = { watch, FSWatcher };

// src/io/streaming-jsonl-reader.ts
import { openSync, readSync, closeSync, statSync } from "fs";
var BUFFER_SIZE = 65536;
function readJsonlStreaming(filePath, callback, options) {
  const startPosition = options?.fromBytePosition ?? 0;
  const result = {
    totalLines: 0,
    processedLines: 0,
    finalBytePosition: 0,
    lastTerminatedPosition: startPosition,
    terminatedLineCount: 0,
    errorCount: 0
  };
  let fileSize;
  try {
    const stats = statSync(filePath);
    fileSize = stats.size;
  } catch {
    return result;
  }
  if (startPosition >= fileSize) {
    result.finalBytePosition = startPosition;
    return result;
  }
  let fd;
  try {
    fd = openSync(filePath, "r");
  } catch {
    return result;
  }
  try {
    const buffer = Buffer.alloc(BUFFER_SIZE);
    let fileOffset = startPosition;
    let lineIndex = 0;
    let leftoverBuf = null;
    while (fileOffset < fileSize) {
      const bytesToRead = Math.min(BUFFER_SIZE, fileSize - fileOffset);
      const bytesRead = readSync(fd, buffer, 0, bytesToRead, fileOffset);
      if (bytesRead === 0) break;
      let workBuf;
      let leftoverLen;
      if (leftoverBuf && leftoverBuf.length > 0) {
        leftoverLen = leftoverBuf.length;
        workBuf = Buffer.concat([leftoverBuf, buffer.subarray(0, bytesRead)]);
      } else {
        leftoverLen = 0;
        workBuf = buffer.subarray(0, bytesRead);
      }
      const workBufFileStart = fileOffset - leftoverLen;
      fileOffset += bytesRead;
      let scanFrom = 0;
      while (scanFrom < workBuf.length) {
        const newlinePos = workBuf.indexOf(10, scanFrom);
        if (newlinePos === -1) {
          leftoverBuf = Buffer.from(workBuf.subarray(scanFrom));
          scanFrom = workBuf.length;
        } else {
          const lineBytes = workBuf.subarray(scanFrom, newlinePos);
          const lineStr = lineBytes.toString("utf-8").trim();
          const lineByteOffset = workBufFileStart + scanFrom;
          const lineEndByteOffset = workBufFileStart + newlinePos + 1;
          result.lastTerminatedPosition = lineEndByteOffset;
          if (lineStr.length > 0) {
            result.totalLines++;
            result.terminatedLineCount++;
            try {
              const entry = JSON.parse(lineStr);
              callback(
                entry,
                lineIndex,
                lineByteOffset,
                lineEndByteOffset,
                /* terminated */
                true
              );
              result.processedLines++;
            } catch (error) {
              result.errorCount++;
              options?.onError?.(lineIndex, error instanceof Error ? error.message : String(error));
            }
            lineIndex++;
          }
          scanFrom = newlinePos + 1;
          leftoverBuf = null;
        }
      }
    }
    if (leftoverBuf && leftoverBuf.length > 0) {
      const finalStr = leftoverBuf.toString("utf-8").trim();
      if (finalStr.length > 0) {
        result.totalLines++;
        const lineByteOffset = fileOffset - leftoverBuf.length;
        const lineEndByteOffset = fileOffset;
        try {
          const entry = JSON.parse(finalStr);
          callback(
            entry,
            lineIndex,
            lineByteOffset,
            lineEndByteOffset,
            /* terminated */
            false
          );
          result.processedLines++;
        } catch (error) {
          result.errorCount++;
          options?.onError?.(lineIndex, error instanceof Error ? error.message : String(error));
        }
      }
    }
    result.finalBytePosition = fileOffset;
  } finally {
    closeSync(fd);
  }
  return result;
}

// src/io/file-service.ts
var FileServiceImpl = class extends EventEmitter2 {
  directoryWatchers = /* @__PURE__ */ new Map();
  fileWatchers = /* @__PURE__ */ new Map();
  watchDirectory(id, options) {
    if (this.directoryWatchers.has(id) || this.fileWatchers.has(id)) {
      this.unwatch(id);
    }
    const watcher = esm_default.watch(options.patterns, {
      persistent: true,
      ignoreInitial: options.ignoreInitial ?? true,
      awaitWriteFinish: options.awaitWriteFinish === true ? { stabilityThreshold: 300, pollInterval: 100 } : options.awaitWriteFinish === false ? false : options.awaitWriteFinish ?? { stabilityThreshold: 300, pollInterval: 100 },
      depth: options.depth
    });
    watcher.on("add", (path2, stats) => this.emitChange(id, "add", path2, stats));
    watcher.on("change", (path2, stats) => this.emitChange(id, "change", path2, stats));
    watcher.on("unlink", (path2) => this.emitChange(id, "unlink", path2));
    watcher.on("error", (error) => this.emit("error", { watcherId: id, error }));
    watcher.on("ready", () => this.emit("ready", { watcherId: id }));
    this.directoryWatchers.set(id, watcher);
  }
  watchFile(id, path2, options) {
    if (this.directoryWatchers.has(id) || this.fileWatchers.has(id)) {
      this.unwatch(id);
    }
    try {
      const watcher = fsWatch(path2, { persistent: options?.persistent ?? true }, (eventType) => {
        if (eventType === "change") {
          const stats = this.getStats(path2);
          this.emitChange(id, "change", path2, stats ?? void 0);
        }
      });
      watcher.on("error", (error) => this.emit("error", { watcherId: id, path: path2, error }));
      this.fileWatchers.set(id, watcher);
    } catch (error) {
      this.emit("error", { watcherId: id, path: path2, error });
    }
  }
  unwatch(id) {
    const dirWatcher = this.directoryWatchers.get(id);
    if (dirWatcher) {
      dirWatcher.close();
      this.directoryWatchers.delete(id);
    }
    const fileWatcher = this.fileWatchers.get(id);
    if (fileWatcher) {
      fileWatcher.close();
      this.fileWatchers.delete(id);
    }
  }
  unwatchAll() {
    for (const [id] of this.directoryWatchers) {
      this.unwatch(id);
    }
    for (const [id] of this.fileWatchers) {
      this.unwatch(id);
    }
  }
  getActiveWatchers() {
    return [...this.directoryWatchers.keys(), ...this.fileWatchers.keys()];
  }
  emitChange(watcherId, event, path2, stats) {
    let fileStats;
    if (stats) {
      const isDir = typeof stats.isDirectory === "function" ? stats.isDirectory() : stats.isDirectory;
      fileStats = {
        size: stats.size,
        mtimeMs: "mtimeMs" in stats ? stats.mtimeMs : stats.mtime?.getTime() ?? 0,
        isDirectory: isDir
      };
    }
    const change = { watcherId, event, path: path2, stats: fileStats };
    this.emit("change", change);
  }
  async readFile(path2, options) {
    return readFile(path2, options?.encoding ?? "utf-8");
  }
  readFileSync(path2, options) {
    return readFileSync(path2, options?.encoding ?? "utf-8");
  }
  async readJson(path2) {
    try {
      if (!this.exists(path2)) return null;
      const content = await this.readFile(path2);
      return JSON.parse(content);
    } catch (error) {
      this.emit("error", { path: path2, error });
      return null;
    }
  }
  readJsonSync(path2) {
    try {
      if (!this.exists(path2)) return null;
      const content = this.readFileSync(path2);
      return JSON.parse(content);
    } catch (error) {
      this.emit("error", { path: path2, error });
      return null;
    }
  }
  async readJsonl(path2) {
    return this.readJsonlSync(path2);
  }
  readJsonlSync(path2) {
    const result = { entries: [], errors: [], totalLines: 0 };
    if (!this.exists(path2)) return result;
    try {
      const content = this.readFileSync(path2);
      const lines = content.split(/\r?\n/);
      for (let i = 0; i < lines.length; i++) {
        const line = lines[i].trim();
        if (!line) continue;
        result.totalLines++;
        try {
          result.entries.push(JSON.parse(line));
        } catch (error) {
          result.errors.push({
            line: i + 1,
            error: error instanceof Error ? error.message : String(error)
          });
        }
      }
    } catch (error) {
      this.emit("error", { path: path2, error });
    }
    return result;
  }
  readFirstLine(path2, maxBytes = 8192) {
    if (!this.exists(path2)) return null;
    try {
      const fd = openSync2(path2, "r");
      const buffer = Buffer.alloc(maxBytes);
      const bytesRead = readSync2(fd, buffer, 0, maxBytes, 0);
      closeSync2(fd);
      const content = buffer.subarray(0, bytesRead).toString("utf-8");
      const newlineIndex = content.indexOf("\n");
      return newlineIndex !== -1 ? content.substring(0, newlineIndex).replace(/\r$/, "") : content;
    } catch (error) {
      this.emit("error", { path: path2, error });
      return null;
    }
  }
  readBytes(path2, options) {
    const fd = openSync2(path2, "r");
    const buffer = Buffer.alloc(options.length);
    readSync2(fd, buffer, 0, options.length, options.start);
    closeSync2(fd);
    return buffer;
  }
  readLastBytes(path2, bytes) {
    const stats = this.getStats(path2);
    if (!stats) return Buffer.alloc(0);
    const start = Math.max(0, stats.size - bytes);
    const length = Math.min(bytes, stats.size);
    return this.readBytes(path2, { start, length });
  }
  readJsonlIncremental(path2, fromPosition) {
    const result = { entries: [], newPosition: fromPosition, errors: [] };
    const stats = this.getStats(path2);
    if (!stats || stats.size <= fromPosition) return result;
    try {
      const bytesToRead = stats.size - fromPosition;
      const buffer = this.readBytes(path2, { start: fromPosition, length: bytesToRead });
      const content = buffer.toString("utf-8");
      const lines = content.split("\n");
      let processedBytes = 0;
      for (let i = 0; i < lines.length; i++) {
        const line = lines[i];
        processedBytes += Buffer.byteLength(line, "utf-8") + 1;
        const trimmed = line.trim();
        if (!trimmed) continue;
        if (i === lines.length - 1 && !content.endsWith("\n")) {
          processedBytes -= Buffer.byteLength(line, "utf-8") + 1;
          break;
        }
        try {
          result.entries.push(JSON.parse(trimmed));
        } catch (error) {
          result.errors.push({
            line: i,
            error: error instanceof Error ? error.message : String(error)
          });
        }
      }
      result.newPosition = fromPosition + processedBytes;
    } catch (error) {
      this.emit("error", { path: path2, error });
    }
    return result;
  }
  readJsonlStreaming(path2, callback, options) {
    return readJsonlStreaming(path2, callback, options);
  }
  async writeFile(path2, content) {
    await this.ensureDir(dirname3(path2));
    await writeFile(path2, content);
  }
  writeFileSync(path2, content) {
    this.ensureDirSync(dirname3(path2));
    writeFileSync(path2, content);
  }
  async writeJson(path2, data) {
    await this.writeFile(path2, JSON.stringify(data, null, 2));
  }
  writeJsonSync(path2, data) {
    this.writeFileSync(path2, JSON.stringify(data, null, 2));
  }
  async appendFile(path2, content) {
    await this.ensureDir(dirname3(path2));
    await appendFile(path2, content);
  }
  appendFileSync(path2, content) {
    this.ensureDirSync(dirname3(path2));
    appendFileSync(path2, content);
  }
  async appendJsonl(path2, entry) {
    await this.appendFile(path2, JSON.stringify(entry) + "\n");
  }
  async ensureDir(path2) {
    if (!existsSync(path2)) {
      mkdirSync(path2, { recursive: true });
    }
  }
  ensureDirSync(path2) {
    if (!existsSync(path2)) {
      mkdirSync(path2, { recursive: true });
    }
  }
  async scanDirectory(path2, options) {
    return this.scanDirectorySync(path2, options);
  }
  scanDirectorySync(path2, options, currentDepth = 0) {
    if (!this.exists(path2)) return [];
    if (options?.maxDepth !== void 0 && currentDepth > options.maxDepth) {
      return [];
    }
    const entries = readdirSync(path2, { withFileTypes: true });
    const results = [];
    for (const entry of entries) {
      const fullPath = join3(path2, entry.name);
      if (entry.isDirectory()) {
        if (options?.includeDirectories || options?.directoriesOnly) {
          if (!options.pattern || this.matchPattern(entry.name, options.pattern)) {
            results.push(fullPath);
          }
        }
        if (options?.recursive) {
          results.push(...this.scanDirectorySync(fullPath, options, currentDepth + 1));
        }
      } else if (!options?.directoriesOnly) {
        if (!options?.pattern || this.matchPattern(entry.name, options.pattern)) {
          results.push(fullPath);
        }
      }
    }
    return results;
  }
  matchPattern(filename, pattern) {
    const braceMatch = pattern.match(/\{([^}]+)\}/);
    if (braceMatch) {
      const alternatives = braceMatch[1].split(",");
      return alternatives.some((alt) => {
        const expandedPattern = pattern.replace(braceMatch[0], alt);
        return this.matchPattern(filename, expandedPattern);
      });
    }
    let regex = pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\\\*/g, "___STAR___").replace(/\\\?/g, "___QUESTION___").replace(/\*/g, "[^/]*").replace(/\?/g, ".").replace(/___STAR___/g, "\\*").replace(/___QUESTION___/g, "\\?");
    regex = `^${regex}$`;
    try {
      return new RegExp(regex).test(filename);
    } catch {
      return filename === pattern;
    }
  }
  exists(path2) {
    return existsSync(path2);
  }
  getStats(path2) {
    try {
      const stats = statSync2(path2);
      return {
        size: stats.size,
        mtimeMs: stats.mtimeMs,
        isDirectory: stats.isDirectory()
      };
    } catch {
      return null;
    }
  }
  getFileSize(path2) {
    const stats = this.getStats(path2);
    return stats?.size ?? null;
  }
  async deleteFile(path2) {
    if (this.exists(path2)) {
      await unlink(path2);
    }
  }
  async cleanupOldFiles(directory, options) {
    const files = await this.scanDirectory(directory, { pattern: options.pattern });
    if (files.length === 0) return 0;
    const fileInfos = files.map((f) => ({ path: f, stats: this.getStats(f) })).filter((f) => f.stats !== null).sort((a, b) => (b.stats?.mtimeMs ?? 0) - (a.stats?.mtimeMs ?? 0));
    let deleted = 0;
    const now = Date.now();
    const maxAgeMs = options.maxAgeDays ? options.maxAgeDays * 24 * 60 * 60 * 1e3 : null;
    for (let i = 0; i < fileInfos.length; i++) {
      const file = fileInfos[i];
      let shouldDelete = false;
      if (options.maxFiles !== void 0 && i >= options.maxFiles) {
        shouldDelete = true;
      }
      if (maxAgeMs && file.stats && now - file.stats.mtimeMs > maxAgeMs) {
        shouldDelete = true;
      }
      if (shouldDelete) {
        await this.deleteFile(file.path);
        deleted++;
      }
    }
    return deleted;
  }
};
function createFileService() {
  return new FileServiceImpl();
}

// src/sources/claude-code/parser/project-parser.ts
import * as path from "node:path";

// src/sources/claude-code/session-metadata.ts
var SYNTHETIC_USER_PREFIXES = [
  "<local-command-caveat>",
  "<local-command-stdout>",
  "<command-name>",
  "<command-message>",
  "<task-notification>",
  "<system-reminder>",
  "<ide_opened_file>",
  "<ide_selection>"
];
function nonEmptyString(value) {
  if (typeof value !== "string") return void 0;
  const trimmed = value.trim();
  return trimmed ? trimmed : void 0;
}
function firstTextContent(record) {
  const message = record.message;
  const content = message?.content;
  if (typeof content === "string") return nonEmptyString(content);
  if (!Array.isArray(content)) return void 0;
  for (const candidate of content) {
    if (!candidate || typeof candidate !== "object") continue;
    const block = candidate;
    if (block.type === "text") {
      const text = nonEmptyString(block.text);
      if (text) return text;
    }
  }
  return void 0;
}
function isClaudeSyntheticPromptText(value) {
  const trimmed = value.trim();
  if (!trimmed) return true;
  const normalized = trimmed.toLowerCase();
  return SYNTHETIC_USER_PREFIXES.some((prefix) => normalized.startsWith(prefix));
}
function extractClaudeHumanPrompt(raw) {
  if (!raw || typeof raw !== "object") return void 0;
  const record = raw;
  if (record.type !== "user") return void 0;
  if (record.isMeta === true || record.isSidechain === true || record.isCompactSummary === true || record.isVisibleInTranscriptOnly === true) {
    return void 0;
  }
  const text = firstTextContent(record);
  if (!text) return void 0;
  if (isClaudeSyntheticPromptText(text)) return void 0;
  return text.slice(0, 200);
}

// src/sources/claude-code/parser/filename-conventions.ts
var SUBAGENT_FILENAME = /^agent-(a.+)\.jsonl$/;
var TODO_FILENAME = /^(.+?)-agent-(.+)\.json$/;
var FILE_HISTORY_FILENAME = /^([0-9a-f]+)@v(\d+)(?:\..*)?$/;
var PLAN_FILENAME = /^(.+)\.md$/;
function parseSubagentFilename(basename4) {
  const match = basename4.match(SUBAGENT_FILENAME);
  if (!match) return null;
  return {
    agentId: match[1],
    agentType: inferSubagentType(basename4)
  };
}
function inferSubagentType(basename4) {
  if (basename4.includes("prompt_suggestion")) return "prompt_suggestion";
  if (basename4.includes("compact")) return "compact";
  return "task";
}
function parseTodoFilename(basename4) {
  const match = basename4.match(TODO_FILENAME);
  if (!match) return null;
  return {
    sessionId: match[1],
    agentId: match[2]
  };
}
function parseFileHistoryFilename(basename4) {
  const match = basename4.match(FILE_HISTORY_FILENAME);
  if (!match) return null;
  const version = parseInt(match[2], 10);
  if (Number.isNaN(version)) return null;
  return {
    hash: match[1],
    version,
    fileName: basename4
  };
}
function parsePlanFilename(basename4) {
  const match = basename4.match(PLAN_FILENAME);
  if (!match) return null;
  return { slug: match[1] };
}

// src/sources/claude-code/parser/project-parser.ts
function slugShape(slug) {
  if (slug.length >= 3 && /[A-Za-z]/.test(slug[0]) && (slug[1] === "-" || slug[1] === ":") && slug[2] === "-") {
    return { prefix: `${slug[0].toUpperCase()}:\\`, sep: "\\", rest: slug.slice(3) };
  }
  if (slug.startsWith("-")) {
    return { prefix: "/", sep: "/", rest: slug.slice(1) };
  }
  return { prefix: "", sep: "/", rest: slug };
}
var ProjectParserImpl = class {
  constructor(fileService2) {
    this.fileService = fileService2;
  }
  // Cached plan index to avoid re-scanning plan files for every project.
  // Keyed by rootDir so it auto-invalidates if the directory changes.
  cachedPlanIndex = null;
  cachedPlanIndexRootDir = null;
  /**
   * Get or build the plan index, caching it for the lifetime of this parser
   * instance (i.e. for one full cold/warm start cycle).
   */
  getPlanIndex(rootDir) {
    if (this.cachedPlanIndex && this.cachedPlanIndexRootDir === rootDir) {
      return this.cachedPlanIndex;
    }
    this.cachedPlanIndex = this.buildPlanIndex(rootDir);
    this.cachedPlanIndexRootDir = rootDir;
    return this.cachedPlanIndex;
  }
  parseAllProjects(rootDir, options) {
    const projectsDir = path.join(rootDir, "projects");
    const projects = [];
    const planIndex = this.getPlanIndex(rootDir);
    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, {
        directoriesOnly: true
      });
      for (const projectPath of projectPaths) {
        try {
          const slug = path.basename(projectPath);
          const project = this.parseProjectInternal(rootDir, slug, options, planIndex);
          if (project) projects.push(project);
        } catch {
        }
      }
    } catch {
    }
    return projects;
  }
  parseAllProjectsStreaming(rootDir, sink, options) {
    const projectsDir = path.join(rootDir, "projects");
    const planIndex = this.getPlanIndex(rootDir);
    for (const [planSlug, plan] of planIndex) {
      sink.onPlan(planSlug, plan);
    }
    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, {
        directoriesOnly: true
      });
      for (const projectPath of projectPaths) {
        try {
          const slug = path.basename(projectPath);
          this.parseProjectStreamingInternal(rootDir, slug, sink, options, planIndex);
        } catch {
        }
      }
    } catch {
    }
  }
  parseProjectStreaming(rootDir, slug, sink, options) {
    const planIndex = this.getPlanIndex(rootDir);
    for (const [planSlug, plan] of planIndex) {
      sink.onPlan(planSlug, plan);
    }
    this.parseProjectStreamingInternal(rootDir, slug, sink, options, planIndex);
  }
  parseProjectStreamingInternal(rootDir, slug, sink, options, _planIndex) {
    const projectDir = path.join(rootDir, "projects", slug);
    const sessionsIndex = this.parseSessionsIndex(projectDir);
    const originalPath = sessionsIndex.originalPath ?? this.slugToPath(slug);
    const skipMessages = options?.skipSessionMessages ?? false;
    sink.onProject(slug, originalPath, sessionsIndex);
    const memory = this.parseProjectMemory(slug, projectDir);
    if (memory) {
      sink.onProjectMemory(slug, memory.content);
    }
    for (const entry of sessionsIndex.entries) {
      try {
        const sessionId = entry.sessionId;
        sink.onSession(slug, entry);
        if (!skipMessages) {
          const canonicalPath = path.join(projectDir, `${sessionId}.jsonl`);
          const filePath = this.fileService.exists(canonicalPath) ? canonicalPath : entry.fullPath && this.fileService.exists(entry.fullPath) ? entry.fullPath : canonicalPath;
          let messageCount = 0;
          let lastBytePosition = 0;
          try {
            const streamResult = this.fileService.readJsonlStreaming(
              filePath,
              (message, index, byteOffset) => {
                sink.onMessage(slug, sessionId, message, index, byteOffset);
                messageCount++;
                lastBytePosition = byteOffset;
              }
            );
            lastBytePosition = streamResult.finalBytePosition;
          } catch {
          }
          const subagents = this.parseSubagents(projectDir, sessionId);
          for (const subagent of subagents) {
            sink.onSubagent(slug, sessionId, subagent);
          }
          const workflows = this.parseWorkflows(projectDir, sessionId);
          for (const workflow of workflows) {
            sink.onWorkflow(slug, sessionId, workflow);
          }
          const toolResults = this.parseToolResults(projectDir, sessionId);
          for (const toolResult of toolResults) {
            sink.onToolResult(slug, sessionId, toolResult);
          }
          sink.onSessionComplete(slug, sessionId, messageCount, lastBytePosition);
        } else {
          sink.onSessionComplete(slug, sessionId, 0, 0);
        }
        const fileHistory = this.parseFileHistory(rootDir, sessionId);
        if (fileHistory) {
          sink.onFileHistory(sessionId, fileHistory);
        }
        const todos = this.parseTodos(rootDir, sessionId);
        for (const todo of todos) {
          sink.onTodo(sessionId, todo);
        }
        const task = this.parseTask(rootDir, sessionId);
        if (task) {
          sink.onTask(sessionId, task);
        }
      } catch {
      }
    }
    sink.onProjectComplete(slug);
  }
  parseProject(rootDir, slug, options) {
    const planIndex = this.getPlanIndex(rootDir);
    return this.parseProjectInternal(rootDir, slug, options, planIndex);
  }
  parseProjectInternal(rootDir, slug, options, planIndex) {
    const projectDir = path.join(rootDir, "projects", slug);
    const sessionsIndex = this.parseSessionsIndex(projectDir);
    const originalPath = sessionsIndex.originalPath ?? this.slugToPath(slug);
    const sessions = [];
    for (const entry of sessionsIndex.entries) {
      try {
        const session = this.buildSession(rootDir, projectDir, slug, entry, options, planIndex);
        sessions.push(session);
      } catch {
      }
    }
    const memory = this.parseProjectMemory(slug, projectDir);
    return { slug, originalPath, sessionsIndex, sessions, memory };
  }
  parseSession(rootDir, slug, sessionId) {
    const projectDir = path.join(rootDir, "projects", slug);
    const sessionsIndex = this.parseSessionsIndex(projectDir);
    const entry = sessionsIndex.entries.find((e) => e.sessionId === sessionId);
    if (!entry) return null;
    const planIndex = this.getPlanIndex(rootDir);
    try {
      return this.buildSession(rootDir, projectDir, slug, entry, void 0, planIndex);
    } catch {
      return null;
    }
  }
  buildSession(rootDir, projectDir, slug, entry, options, planIndex) {
    const sessionId = entry.sessionId;
    const skipMessages = options?.skipSessionMessages ?? false;
    const messages = skipMessages ? [] : this.parseSessionMessages(projectDir, sessionId, entry.fullPath);
    const planSlug = messages.length > 0 ? this.extractPlanSlugFromMessages(messages, planIndex) : this.peekPlanSlug(projectDir, sessionId, planIndex);
    return {
      sessionId,
      indexEntry: entry,
      messages,
      subagents: skipMessages ? [] : this.parseSubagents(projectDir, sessionId),
      workflows: skipMessages ? [] : this.parseWorkflows(projectDir, sessionId),
      toolResults: skipMessages ? [] : this.parseToolResults(projectDir, sessionId),
      fileHistory: this.parseFileHistory(rootDir, sessionId),
      todos: this.parseTodos(rootDir, sessionId),
      task: this.parseTask(rootDir, sessionId),
      plan: planSlug ? planIndex.get(planSlug) ?? null : null
    };
  }
  parseSessionsIndex(projectDir) {
    try {
      const index = this.fileService.readJsonSync(path.join(projectDir, "sessions-index.json"));
      if (index && index.entries.length > 0) {
        const merged = this.mergeWithDiscoveredEntries(index.entries, projectDir, index.originalPath);
        return { ...index, entries: merged };
      }
      if (index?.originalPath) {
        return { ...index, entries: this.discoverSessionEntries(projectDir, index.originalPath) };
      }
    } catch {
    }
    return {
      version: 1,
      entries: this.discoverSessionEntries(projectDir, void 0)
    };
  }
  /**
   * Merge entries from sessions-index.json with JSONL files discovered on
   * disk.  Any on-disk JSONL file whose session ID is NOT already in the
   * index gets a freshly-built entry appended.  This handles the common case
   * where the index is stale (e.g. after a Claude upgrade or migration).
   */
  mergeWithDiscoveredEntries(indexEntries, projectDir, originalPath) {
    const indexedIds = new Set(indexEntries.map((e) => e.sessionId));
    const discovered = this.discoverSessionEntries(projectDir, originalPath);
    const extra = discovered.filter((e) => !indexedIds.has(e.sessionId));
    if (extra.length === 0) return indexEntries;
    return [...indexEntries, ...extra];
  }
  discoverSessionEntries(projectDir, originalPath) {
    const UUID_JSONL = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$/;
    const entries = [];
    let filePaths;
    try {
      filePaths = this.fileService.scanDirectorySync(projectDir, { pattern: "*.jsonl" });
    } catch {
      return entries;
    }
    for (const filePath of filePaths) {
      const fileName = path.basename(filePath);
      if (!UUID_JSONL.test(fileName)) continue;
      const sessionId = fileName.replace(".jsonl", "");
      const stats = this.fileService.getStats(filePath);
      if (!stats) continue;
      let firstPrompt = "";
      try {
        this.fileService.readJsonlStreaming(filePath, (msg) => {
          if (firstPrompt) return;
          const candidate = extractClaudeHumanPrompt(msg);
          if (candidate) {
            firstPrompt = candidate;
          }
        });
      } catch {
      }
      const modifiedIso = new Date(stats.mtimeMs).toISOString();
      entries.push({
        sessionId,
        fullPath: filePath,
        fileMtime: stats.mtimeMs,
        firstPrompt: firstPrompt || "No prompt",
        summary: "",
        messageCount: 0,
        created: modifiedIso,
        modified: modifiedIso,
        gitBranch: "",
        projectPath: originalPath ?? this.slugToPath(path.basename(projectDir)),
        isSidechain: false
      });
    }
    return entries;
  }
  parseSessionMessages(projectDir, sessionId, fullPath) {
    try {
      const canonicalPath = path.join(projectDir, `${sessionId}.jsonl`);
      const filePath = this.fileService.exists(canonicalPath) ? canonicalPath : fullPath && this.fileService.exists(fullPath) ? fullPath : canonicalPath;
      const result = this.fileService.readJsonlSync(filePath);
      return result.entries;
    } catch {
      return [];
    }
  }
  parseSubagents(projectDir, sessionId) {
    const subagentsDir = path.join(projectDir, sessionId, "subagents");
    const transcripts = [];
    try {
      const filePaths = this.fileService.scanDirectorySync(subagentsDir, { pattern: "*.jsonl" });
      for (const filePath of filePaths) {
        const transcript = this.readSubagentTranscript(filePath, "");
        if (transcript) transcripts.push(transcript);
      }
    } catch {
    }
    try {
      const workflowsDir = path.join(subagentsDir, "workflows");
      const wfDirs = this.fileService.scanDirectorySync(workflowsDir, { directoriesOnly: true });
      for (const wfDir of wfDirs) {
        const workflowId = path.basename(wfDir);
        try {
          const agentFiles = this.fileService.scanDirectorySync(wfDir, { pattern: "agent-*.jsonl" });
          for (const filePath of agentFiles) {
            const transcript = this.readSubagentTranscript(filePath, workflowId);
            if (transcript) transcripts.push(transcript);
          }
        } catch {
        }
      }
    } catch {
    }
    return transcripts;
  }
  readSubagentTranscript(filePath, workflowId) {
    try {
      const fileName = path.basename(filePath);
      const result = this.fileService.readJsonlSync(filePath);
      const meta = this.fileService.readJsonSync(filePath.replace(/\.jsonl$/, ".meta.json"));
      return {
        agentId: this.extractAgentId(fileName),
        agentType: this.inferAgentType(fileName),
        fileName,
        messages: result.entries,
        workflowId,
        ...meta ? { meta } : {}
      };
    } catch {
      return null;
    }
  }
  /**
   * Parse the workflow run records under `projects/{slug}/{sid}/workflows/`.
   * Each `wf_*.json` is an agent-orchestration run record; its `journal.jsonl`
   * (started/result events) lives beside the run's nested transcripts under
   * `subagents/workflows/{runId}/`.
   */
  parseWorkflows(projectDir, sessionId) {
    const workflowsDir = path.join(projectDir, sessionId, "workflows");
    const runs = [];
    try {
      const filePaths = this.fileService.scanDirectorySync(workflowsDir, { pattern: "wf_*.json" });
      for (const filePath of filePaths) {
        try {
          const data = this.fileService.readJsonSync(filePath);
          if (!data) continue;
          const workflowId = typeof data.runId === "string" && data.runId || path.basename(filePath, ".json");
          const num = (v) => typeof v === "number" ? v : 0;
          runs.push({
            workflowId,
            name: typeof data.workflowName === "string" && data.workflowName || workflowId,
            status: typeof data.status === "string" ? data.status : "",
            agentCount: num(data.agentCount),
            totalTokens: num(data.totalTokens),
            totalToolCalls: num(data.totalToolCalls),
            durationMs: num(data.durationMs),
            subagentCount: this.countWorkflowSubagents(projectDir, sessionId, workflowId),
            data,
            journal: this.parseWorkflowJournal(projectDir, sessionId, workflowId)
          });
        } catch {
        }
      }
    } catch {
    }
    return runs.sort((a, b) => a.workflowId.localeCompare(b.workflowId));
  }
  countWorkflowSubagents(projectDir, sessionId, workflowId) {
    try {
      const dir = path.join(projectDir, sessionId, "subagents", "workflows", workflowId);
      return this.fileService.scanDirectorySync(dir, { pattern: "agent-*.jsonl" }).length;
    } catch {
      return 0;
    }
  }
  parseWorkflowJournal(projectDir, sessionId, workflowId) {
    try {
      const journalPath = path.join(projectDir, sessionId, "subagents", "workflows", workflowId, "journal.jsonl");
      return this.fileService.readJsonlSync(journalPath).entries;
    } catch {
      return [];
    }
  }
  extractAgentId(fileName) {
    const parsed = parseSubagentFilename(fileName);
    return parsed ? parsed.agentId : fileName.replace(/\.jsonl$/, "");
  }
  inferAgentType(fileName) {
    return inferSubagentType(fileName);
  }
  parseToolResults(projectDir, sessionId) {
    const resultsDir = path.join(projectDir, sessionId, "tool-results");
    const results = [];
    try {
      const filePaths = this.fileService.scanDirectorySync(resultsDir, { pattern: "*.txt" });
      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const toolUseId = fileName.replace(/\.txt$/, "");
          const content = this.fileService.readFileSync(filePath);
          results.push({ toolUseId, content });
        } catch {
        }
      }
    } catch {
    }
    return results;
  }
  parseProjectMemory(projectSlug, projectDir) {
    try {
      const content = this.fileService.readFileSync(path.join(projectDir, "memory", "MEMORY.md"));
      return { projectSlug, content };
    } catch {
      return null;
    }
  }
  parseFileHistory(rootDir, sessionId) {
    const historyDir = path.join(rootDir, "file-history", sessionId);
    try {
      const filePaths = this.fileService.scanDirectorySync(historyDir);
      if (filePaths.length === 0) return null;
      const snapshots = [];
      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const parsed = parseFileHistoryFilename(fileName);
          if (!parsed) continue;
          const content = this.fileService.readFileSync(filePath);
          const stats = this.fileService.getStats(filePath);
          snapshots.push({
            hash: parsed.hash,
            version: parsed.version,
            fileName,
            content,
            size: stats?.size ?? 0
          });
        } catch {
        }
      }
      return snapshots.length > 0 ? { sessionId, snapshots } : null;
    } catch {
      return null;
    }
  }
  parseTodos(rootDir, sessionId) {
    const todosDir = path.join(rootDir, "todos");
    const todoFiles = [];
    try {
      const filePaths = this.fileService.scanDirectorySync(todosDir, {
        pattern: `${sessionId}-agent-*.json`
      });
      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const parsed = parseTodoFilename(fileName);
          if (!parsed) continue;
          const items = this.fileService.readJsonSync(filePath) ?? [];
          todoFiles.push({
            sessionId: parsed.sessionId,
            agentId: parsed.agentId,
            items: Array.isArray(items) ? items : []
          });
        } catch {
        }
      }
    } catch {
    }
    return todoFiles;
  }
  parseTask(rootDir, sessionId) {
    const taskDir = path.join(rootDir, "tasks", sessionId);
    try {
      const lockExists = this.fileService.exists(path.join(taskDir, ".lock"));
      if (!lockExists) return null;
      let hasHighwatermark = false;
      let highwatermark = null;
      try {
        const hwContent = this.fileService.readFileSync(path.join(taskDir, ".highwatermark"));
        hasHighwatermark = true;
        highwatermark = parseInt(hwContent.trim(), 10);
        if (isNaN(highwatermark)) highwatermark = null;
      } catch {
      }
      return { taskId: sessionId, hasHighwatermark, highwatermark, lockExists: true };
    } catch {
      return null;
    }
  }
  buildPlanIndex(rootDir) {
    const index = /* @__PURE__ */ new Map();
    const plansDir = path.join(rootDir, "plans");
    try {
      const filePaths = this.fileService.scanDirectorySync(plansDir, { pattern: "*.md" });
      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const parsed = parsePlanFilename(fileName);
          if (!parsed) continue;
          const planSlug = parsed.slug;
          const content = this.fileService.readFileSync(filePath);
          const stats = this.fileService.getStats(filePath);
          const titleMatch = content.match(/^#\s+(.+)$/m);
          const title = titleMatch ? titleMatch[1] : planSlug;
          index.set(planSlug, { slug: planSlug, title, content, size: stats?.size ?? 0 });
        } catch {
        }
      }
    } catch {
    }
    return index;
  }
  extractPlanSlugFromMessages(messages, planIndex) {
    for (const msg of messages) {
      const raw = msg;
      const slug = raw.slug;
      if (typeof slug === "string" && planIndex.has(slug)) {
        return slug;
      }
    }
    return null;
  }
  peekPlanSlug(projectDir, sessionId, planIndex) {
    if (planIndex.size === 0) return null;
    try {
      const filePath = path.join(projectDir, `${sessionId}.jsonl`);
      const content = this.fileService.readFileSync(filePath);
      const slugPattern = /"slug"\s*:\s*"([^"]+)"/;
      const match = content.match(slugPattern);
      if (match) {
        const candidate = match[1];
        if (planIndex.has(candidate)) return candidate;
      }
    } catch {
    }
    return null;
  }
  slugToPath(slug) {
    const { prefix, sep, rest } = slugShape(slug);
    if (rest === "") return prefix;
    const parts = rest.split("-");
    let resolved = "";
    let i = 0;
    while (i < parts.length) {
      let matched = false;
      for (let end = parts.length; end > i; end--) {
        const candidate = parts.slice(i, end).join("-");
        const probe = resolved === "" ? prefix + candidate : prefix + resolved + sep + candidate;
        if (this.fileService.getStats(probe)) {
          resolved = resolved === "" ? candidate : resolved + sep + candidate;
          i = end;
          matched = true;
          break;
        }
      }
      if (!matched) {
        resolved = resolved === "" ? parts[i] : resolved + sep + parts[i];
        i++;
      }
    }
    return prefix + resolved;
  }
};
function createProjectParser(fileService2) {
  return new ProjectParserImpl(fileService2);
}

// src/workers/parse-worker.ts
if (!parentPort) {
  throw new Error("parse-worker must be run as a worker thread");
}
var fileService = createFileService();
var parser = createProjectParser(fileService);
var port = parentPort;
port.on("message", (msg) => {
  if (msg.type === "shutdown") {
    process.exit(0);
  }
  if (msg.type === "parse-project") {
    const startTime = Date.now();
    const { rootDir, slug } = msg;
    try {
      let messageBatch = [];
      let batchStartIndex = 0;
      let batchByteOffsets = [];
      let currentSlug = "";
      let currentSessionId = "";
      const flushBatch = () => {
        if (messageBatch.length > 0) {
          port.postMessage({
            type: "message-batch",
            slug: currentSlug,
            sessionId: currentSessionId,
            messages: messageBatch,
            startIndex: batchStartIndex,
            byteOffsets: batchByteOffsets
          });
          messageBatch = [];
          batchByteOffsets = [];
        }
      };
      const sink = {
        onProject(slug2, originalPath, sessionsIndex) {
          port.postMessage({
            type: "project-result",
            slug: slug2,
            originalPath,
            sessionsIndexJson: JSON.stringify(sessionsIndex)
          });
        },
        onProjectMemory(slug2, content) {
          port.postMessage({ type: "project-memory", slug: slug2, content });
        },
        onSession(slug2, entry) {
          port.postMessage({
            type: "session-result",
            slug: slug2,
            sessionId: entry.sessionId,
            indexEntryJson: JSON.stringify(entry)
          });
        },
        onMessage(slug2, sessionId, message, index, byteOffset) {
          if (currentSessionId !== sessionId) {
            flushBatch();
            currentSlug = slug2;
            currentSessionId = sessionId;
            batchStartIndex = index;
          }
          messageBatch.push(JSON.stringify(message));
          batchByteOffsets.push(byteOffset);
          if (messageBatch.length >= 500) {
            flushBatch();
            batchStartIndex = index + 1;
          }
        },
        onSubagent(slug2, sessionId, transcript) {
          flushBatch();
          port.postMessage({
            type: "subagent-result",
            slug: slug2,
            sessionId,
            agentId: transcript.agentId,
            agentType: transcript.agentType,
            fileName: transcript.fileName,
            messagesJson: JSON.stringify(transcript.messages),
            messageCount: transcript.messages.length,
            workflowId: transcript.workflowId
          });
        },
        onWorkflow(slug2, sessionId, workflow) {
          flushBatch();
          port.postMessage({
            type: "workflow-result",
            slug: slug2,
            sessionId,
            workflowJson: JSON.stringify(workflow)
          });
        },
        onToolResult(slug2, sessionId, toolResult) {
          port.postMessage({
            type: "tool-result",
            slug: slug2,
            sessionId,
            toolUseId: toolResult.toolUseId,
            content: toolResult.content
          });
        },
        onFileHistory(sessionId, history) {
          port.postMessage({
            type: "file-history",
            sessionId,
            dataJson: JSON.stringify(history)
          });
        },
        onTodo(sessionId, todo) {
          port.postMessage({
            type: "todo-result",
            sessionId,
            agentId: todo.agentId,
            itemsJson: JSON.stringify(todo.items)
          });
        },
        onTask(sessionId, task) {
          port.postMessage({
            type: "task-result",
            sessionId,
            taskJson: JSON.stringify(task)
          });
        },
        onPlan(slug2, plan) {
          port.postMessage({
            type: "plan-result",
            slug: slug2,
            title: plan.title,
            content: plan.content,
            size: plan.size
          });
        },
        onSessionComplete(slug2, sessionId, messageCount, lastBytePosition) {
          flushBatch();
          port.postMessage({
            type: "session-complete",
            slug: slug2,
            sessionId,
            messageCount,
            lastBytePosition
          });
        },
        onProjectComplete(slug2) {
          flushBatch();
          port.postMessage({
            type: "project-complete",
            slug: slug2,
            durationMs: Date.now() - startTime
          });
        }
      };
      parser.parseProjectStreaming(rootDir, slug, sink);
    } catch (err) {
      port.postMessage({
        type: "worker-error",
        slug,
        error: String(err)
      });
    }
  }
});
/*! Bundled license information:

chokidar/esm/index.js:
  (*! chokidar - MIT License (c) 2012 Paul Miller (paulmillr.com) *)
*/
