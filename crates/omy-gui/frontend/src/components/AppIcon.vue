<script setup lang="ts">
/** 全局图标入口。
 *
 * 组件统一使用 Lucide SVG，避免 Emoji 在不同系统上颜色、尺寸和基线不一致。
 * 图标名集中映射：业务组件只传语义名，后续换图标或调整全局线宽无需逐页修改。
 */
import { computed, type Component } from 'vue';
import {
  Archive,
  ArrowLeft,
  Ban,
  Check,
  Circle,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleHelp,
  CircleX,
  ChevronDown,
  ChevronRight,
  Clapperboard,
  Clipboard,
  ClipboardPaste,
  Cloud,
  CornerDownRight,
  Database,
  Download,
  EllipsisVertical,
  ExternalLink,
  Eye,
  FileArchive,
  FileSpreadsheet,
  FileText,
  Folder,
  FolderLock,
  FolderOpen,
  Globe,
  Grid2X2,
  HardDrive,
  House,
  Hourglass,
  Image,
  Inbox,
  Import,
  Info,
  KeyRound,
  Laptop,
  List,
  LoaderCircle,
  LockKeyhole,
  LockOpen,
  MapPin,
  MemoryStick,
  Menu,
  MessageCircle,
  Minus,
  Monitor,
  Music,
  Package,
  PanelTop,
  Pause,
  Pencil,
  Pin,
  Play,
  Plus,
  Presentation,
  QrCode,
  Radio,
  RefreshCw,
  RotateCcw,
  ScanSearch,
  Scissors,
  Search,
  Send,
  Settings,
  ShieldCheck,
  Smartphone,
  Trash2,
  Turtle,
  Upload,
  User,
  Users,
  Wifi,
  Workflow,
  X,
  Zap,
  Camera,
} from '@lucide/vue';

const props = withDefaults(defineProps<{
  name: string;
  size?: number | string;
  strokeWidth?: number | string;
}>(), {
  size: 18,
  strokeWidth: 1.8,
});

const icons: Record<string, Component> = {
  archive: Archive, '🗂️': Archive,
  ban: Ban, '⛔': Ban,
  check: Check, '✓': Check,
  circle: Circle, '🟢': Circle, 'cached': Circle, '●': Circle,
  partial: CircleDashed, '◐': CircleDashed,
  'circle-alert': CircleAlert, '⚠️': CircleAlert,
  'circle-check': CircleCheck,
  help: CircleHelp, '❓': CircleHelp,
  'circle-x': CircleX,
  video: Clapperboard, '🎬': Clapperboard,
  clipboard: Clipboard, '📋': Clipboard,
  paste: ClipboardPaste, '📥': ClipboardPaste,
  cloud: Cloud, '☁️': Cloud,
  database: Database, '🗄️': Database,
  download: Download, '⬇️': Download,
  more: EllipsisVertical, '⋮': EllipsisVertical,
  external: ExternalLink, '📤': ExternalLink,
  eye: Eye, '👁️': Eye,
  'file-archive': FileArchive,
  spreadsheet: FileSpreadsheet,
  file: FileText, '📄': FileText, '📝': FileText, '📕': FileText, '📘': FileText, '📗': FileText, '📙': FileText,
  folder: Folder, '📁': Folder,
  'folder-lock': FolderLock, '🔐': FolderLock,
  'folder-open': FolderOpen, '📂': FolderOpen,
  globe: Globe, '🌐': Globe,
  grid: Grid2X2, '⊞': Grid2X2,
  drive: HardDrive, '💾': HardDrive,
  home: House, '🏠': House,
  hourglass: Hourglass, '⌛': Hourglass,
  image: Image, '🖼️': Image,
  import: Import,
  key: KeyRound, '🔑': KeyRound,
  laptop: Laptop, '💻': Laptop,
  list: List, '☰': List,
  loader: LoaderCircle, '⏳': LoaderCircle,
  lock: LockKeyhole, '🔒': LockKeyhole,
  unlock: LockOpen, '🔓': LockOpen,
  pin: MapPin, '📍': MapPin,
  'memory-stick': MemoryStick, '💳': MemoryStick,
  menu: Menu,
  message: MessageCircle, '💬': MessageCircle,
  minus: Minus, '➖': Minus,
  monitor: Monitor, '🖥️': Monitor,
  music: Music, '🎵': Music,
  package: Package, '📦': Package,
  panel: PanelTop,
  pause: Pause, 'Ⅱ': Pause, '❚❚': Pause,
  pencil: Pencil, '✏️': Pencil, '✎': Pencil,
  'pin-file': Pin, '📌': Pin,
  play: Play, '▶': Play,
  plus: Plus, '➕': Plus,
  presentation: Presentation,
  qr: QrCode, '▦': QrCode,
  radio: Radio, '📡': Radio,
  refresh: RefreshCw, '🔄': RefreshCw,
  retry: RotateCcw, '↻': RotateCcw,
  scan: ScanSearch,
  scissors: Scissors, '✂️': Scissors,
  search: Search, '🔍': Search, '🔎': Search,
  send: Send, '✈️': Send, '↗': Send,
  settings: Settings, '⚙️': Settings,
  shield: ShieldCheck,
  phone: Smartphone, '📱': Smartphone,
  trash: Trash2, '🗑️': Trash2,
  turtle: Turtle, '🐌': Turtle,
  upload: Upload, '⬆️': Upload,
  user: User, '👤': User,
  users: Users, '👥': Users,
  wifi: Wifi,
  transfers: Workflow, '🔀': Workflow,
  close: X, '✕': X,
  fast: Zap, '⚡': Zap,
  camera: Camera, '📷': Camera,
  archiveFile: FileArchive, '🗜️': FileArchive,
  inbox: Inbox, '📭': Inbox,
  info: Info, 'ℹ️': Info,
  arrowLeft: ArrowLeft,
  chevronDown: ChevronDown,
  chevronRight: ChevronRight,
  cornerDownRight: CornerDownRight,
};

const component = computed(() => icons[props.name] || FileText);
</script>

<template>
  <component
    :is="component"
    class="app-icon"
    :size="size"
    :stroke-width="strokeWidth"
    aria-hidden="true"
  />
</template>

<style scoped>
.app-icon {
  display: inline-block;
  flex: none;
  vertical-align: -0.16em;
  color: currentColor;
}
</style>
