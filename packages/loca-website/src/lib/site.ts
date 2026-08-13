// Site-wide navigation config. The navbar tab selector lists these top-level
// pages; the feature showcase is part of the home page (its second section),
// not a page here.

export interface NavPage {
  id: string;
  label: string;
  href: string;
}

export const PAGES: NavPage[] = [
  { id: "home", label: "Home", href: "/" },
  // The one page whose command must work with no JS: the install generator.
  { id: "install", label: "Install", href: "/install" },
  // The manual is many pages; the tab opens its first subsection.
  {
    id: "manual",
    label: "User Manual",
    href: "/manual/getting-started/installing",
  },
  { id: "gallery", label: "Gallery", href: "/gallery" },
];

// Private for now; linked anyway (no star count yet).
export const GITHUB_URL = "https://github.com/rzkyif/loca";

/** Which top-level page a given pathname belongs to, for the navbar knob. */
export function activePageId(pathname: string): string {
  if (pathname.startsWith("/install")) return "install";
  if (pathname.startsWith("/manual")) return "manual";
  if (pathname.startsWith("/gallery")) return "gallery";
  return "home";
}
