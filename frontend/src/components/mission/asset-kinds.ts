/**
 * 资产分类助手 —— 把 Lynceus 的 `MissionAsset.asset_type`（19 种）折叠成
 * 资产页的六类口径，供资产表格与资产覆盖图共用。
 *
 * 映射（资产页口径 → Lynceus wire 值）：
 *   根域名 → `domain`
 *   子域名 → `host`
 *   IP     → `ip`
 *   应用   → `url`
 *   服务   → `service`
 *   接口   → `endpoint` + `api`
 *   其他   → 剩余全部（repository/source_path/binary/cloud_resource/…）
 *
 * 后端 `host_asset_type()` 只区分 IP 与主机名，根域名/子域名由
 * `domain` / `host` 两个类型承载，因此「根域名」桶只收显式 domain 资产；
 * 主机名资产一律进「子域名」桶，其根域名由末两段标签推导（`rootDomainOf`）。
 */
import {
  AppWindow,
  type LucideIcon,
  Globe,
  Link2,
  Network,
  RadioTower,
  Server,
} from 'lucide-react';

import type { MissionAsset } from '@/lib/types';

export type AssetGroupKey =
  | 'root_domain'
  | 'ip'
  | 'subdomain'
  | 'app'
  | 'service'
  | 'endpoint'
  | 'other';

export interface AssetGroup {
  key: AssetGroupKey;
  /** i18n key：`missions.assets.kinds.<suffix>`。 */
  labelKey: string;
  icon: LucideIcon;
  /** 归属本桶的后端 asset_type wire 值。 */
  types: string[];
}

export const ASSET_GROUPS: AssetGroup[] = [
  { key: 'root_domain', labelKey: 'rootDomain', icon: Globe, types: ['domain'] },
  { key: 'ip', labelKey: 'ip', icon: Network, types: ['ip'] },
  { key: 'subdomain', labelKey: 'subdomain', icon: Server, types: ['host'] },
  { key: 'app', labelKey: 'app', icon: AppWindow, types: ['url'] },
  { key: 'service', labelKey: 'service', icon: RadioTower, types: ['service'] },
  { key: 'endpoint', labelKey: 'endpoint', icon: Link2, types: ['endpoint', 'api'] },
];

const OTHER_GROUP: AssetGroup = {
  key: 'other',
  labelKey: 'other',
  icon: Globe,
  types: [],
};

const GROUP_BY_TYPE = new Map<string, AssetGroupKey>();
for (const group of ASSET_GROUPS) {
  for (const type of group.types) GROUP_BY_TYPE.set(type, group.key);
}

/** asset_type → 资产桶；未覆盖的类型进 `other`。 */
export function assetGroupOf(assetType: string): AssetGroupKey {
  return GROUP_BY_TYPE.get(assetType) ?? 'other';
}

/** 资产桶元数据（含 `other`）。 */
export function assetGroup(key: AssetGroupKey): AssetGroup {
  if (key === 'other') return OTHER_GROUP;
  return ASSET_GROUPS.find((group) => group.key === key) ?? OTHER_GROUP;
}

/** 六类 + 其他的完整顺序（tab 与覆盖图共用）。 */
export const ASSET_GROUP_ORDER: AssetGroupKey[] = [
  ...ASSET_GROUPS.map((group) => group.key),
  'other',
];

/* ───────────────────────── metadata 读取 ───────────────────────── */

/** 从 metadata 取字符串；缺失返回 undefined。 */
export function metaString(
  metadata: Record<string, unknown> | undefined,
  key: string,
): string | undefined {
  const value = metadata?.[key];
  if (typeof value === 'string') return value.trim() === '' ? undefined : value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return undefined;
}

/** 从 metadata 取数字；缺失或非法返回 undefined。 */
export function metaNumber(
  metadata: Record<string, unknown> | undefined,
  key: string,
): number | undefined {
  const value = metadata?.[key];
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value);
    if (!Number.isNaN(parsed)) return parsed;
  }
  return undefined;
}

/** 从 metadata 取字符串数组（容忍单字符串）。 */
export function metaStringList(
  metadata: Record<string, unknown> | undefined,
  key: string,
): string[] {
  const value = metadata?.[key];
  if (Array.isArray(value)) {
    return value
      .map((item) => {
        if (typeof item === 'string') return item;
        if (item != null && typeof item === 'object') {
          const record = item as Record<string, unknown>;
          const name = record.name ?? record.value ?? record.key;
          if (typeof name === 'string') return name;
        }
        return '';
      })
      .filter((item) => item !== '');
  }
  const single = metaString(metadata, key);
  return single ? [single] : [];
}

/* ───────────────────────── 取值助手 ───────────────────────── */

/** 主机名/域名的根域名（末两段标签）；IP 或非法输入原样返回。 */
export function rootDomainOf(host: string): string {
  const trimmed = host.trim().toLowerCase().replace(/\.$/, '');
  if (trimmed === '' || /^\d+\.\d+\.\d+\.\d+$/.test(trimmed)) return trimmed;
  const labels = trimmed.split('.');
  if (labels.length <= 2) return trimmed;
  return labels.slice(-2).join('.');
}

/** URL → origin（协议 + 主机 + 端口）；解析失败返回空串。 */
export function urlOrigin(url: string): string {
  try {
    const parsed = new URL(url.trim());
    return parsed.origin;
  } catch {
    return '';
  }
}

/** URL → pathname（含查询）；解析失败返回空串。 */
export function urlPath(url: string): string {
  try {
    const parsed = new URL(url.trim());
    return `${parsed.pathname}${parsed.search}`;
  } catch {
    return '';
  }
}

/** 资产在主列表里的可读主文本。 */
export function assetPrimaryValue(asset: MissionAsset): string {
  if (asset.value && asset.value.trim() !== '') return asset.value;
  if (asset.label && asset.label.trim() !== '') return asset.label;
  return `#${asset.id}`;
}

/** 资产副标题（label 与 value 不同才有意义）。 */
export function assetSecondaryValue(asset: MissionAsset): string | undefined {
  const label = asset.label?.trim();
  if (label && label !== asset.value) return label;
  return undefined;
}

/** 资产是否被任务真正"碰过"（有证据/发现/工具调用任一关联）。 */
export function assetIsExercised(asset: MissionAsset): boolean {
  return (
    asset.evidence_ids.length > 0 ||
    asset.finding_ids.length > 0 ||
    asset.tool_invocation_ids.length > 0
  );
}
