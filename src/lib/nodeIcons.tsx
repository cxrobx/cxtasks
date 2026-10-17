import type { CSSProperties } from "react";
import {
  BookOpen,
  Briefcase,
  Building2,
  Calendar,
  Camera,
  CheckSquare,
  Code,
  DollarSign,
  Film,
  Flame,
  Folder,
  Globe,
  GraduationCap,
  Hammer,
  Headphones,
  Heart,
  House,
  Landmark,
  Leaf,
  Mail,
  Mic,
  Monitor,
  Music,
  Package,
  Palette,
  PenLine,
  PiggyBank,
  Rocket,
  Scale,
  Shield,
  ShoppingBag,
  Smartphone,
  Sparkles,
  Star,
  Target,
  Terminal,
  Truck,
  User,
  Users,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { hueStyle, HUES, type Hue } from "@/lib/hues";

/**
 * The built-in icons a project, area or tag can wear, keyed by the name stored
 * after `lucide:` in its `icon` column.
 *
 * A CURATED set, not all of lucide: a picker of 1,500 glyphs is a search box,
 * and these cover what the portfolio actually is (products, music, clients,
 * money, writing, teaching). The keys are lucide's own kebab-case names so a
 * stored value stays meaningful if the set grows. A name this build does not
 * know — written by a newer one, synced over — falls back to the row's default
 * glyph rather than rendering nothing.
 */
export const BUILTIN_ICONS: Record<string, LucideIcon> = {
  folder: Folder,
  target: Target,
  briefcase: Briefcase,
  "building-2": Building2,
  rocket: Rocket,
  music: Music,
  mic: Mic,
  headphones: Headphones,
  code: Code,
  terminal: Terminal,
  smartphone: Smartphone,
  monitor: Monitor,
  mail: Mail,
  "check-square": CheckSquare,
  calendar: Calendar,
  "dollar-sign": DollarSign,
  "piggy-bank": PiggyBank,
  landmark: Landmark,
  house: House,
  hammer: Hammer,
  "graduation-cap": GraduationCap,
  "book-open": BookOpen,
  "pen-line": PenLine,
  camera: Camera,
  film: Film,
  palette: Palette,
  heart: Heart,
  users: Users,
  user: User,
  globe: Globe,
  "shopping-bag": ShoppingBag,
  package: Package,
  truck: Truck,
  zap: Zap,
  flame: Flame,
  leaf: Leaf,
  star: Star,
  scale: Scale,
  shield: Shield,
  sparkles: Sparkles,
};

export const IMAGE_PREFIX = "data:image/png;base64,";

/** Deliberately a boolean, not a `icon is string` guard: the false branch
 * would narrow a string to `never` and hide every `lucide:` value from TS. */
export function isImageIcon(icon: string | null | undefined): boolean {
  return !!icon && icon.startsWith(IMAGE_PREFIX);
}

/** The stored hue, when it is one this build knows. */
export function storedHue(color: string | null | undefined): Hue | null {
  return color && (HUES as readonly string[]).includes(color) ? (color as Hue) : null;
}

/**
 * One sidebar icon, whatever it is.
 *
 * - An **image** (an app's own icon) draws as-is — colour never touches it.
 * - A **built-in** glyph, or the row's `fallback` when nothing is chosen, takes
 *   the stored colour; failing that `autoHue`; failing that it is ink, in
 *   `inkClass`. So an area with nothing chosen stays ink, exactly as before.
 *
 * `dim` is a closed project: the glyph goes back to ink and an image fades,
 * because colour is how the live rows are told apart.
 */
export function NodeIcon({
  icon,
  color,
  fallback,
  autoHue = null,
  size = 14,
  inkClass = "text-content-muted",
  dim = false,
}: {
  icon: string | null | undefined;
  color: string | null | undefined;
  fallback: LucideIcon;
  autoHue?: Hue | null;
  size?: number;
  inkClass?: string;
  dim?: boolean;
}) {
  if (icon && isImageIcon(icon)) {
    // `alt=""`: the row's own label names it, and a screen reader reading
    // "CXMail CXMail" is noise. Never draggable — the row is the drag handle.
    return (
      <img
        src={icon}
        alt=""
        draggable={false}
        width={size + 2}
        height={size + 2}
        className={cn("-m-px shrink-0 rounded-[3px] object-contain", dim && "opacity-50")}
      />
    );
  }
  const name = icon?.startsWith("lucide:") ? icon.slice("lucide:".length) : null;
  const Glyph = (name && BUILTIN_ICONS[name]) || fallback;
  const hue = dim ? null : storedHue(color) ?? autoHue;
  const style: CSSProperties | undefined = hue ? hueStyle(hue) : undefined;
  return <Glyph size={size} className={cn("shrink-0", !hue && inkClass)} style={style} />;
}
