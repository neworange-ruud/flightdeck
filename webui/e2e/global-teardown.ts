import { stopHost } from "./support/host";

/** Kill the desktop and remove its throwaway `$HOME` and fixture repo. Set
 * `FD_E2E_KEEP_TMP=1` to keep them (and the PTY transcript) for a post-mortem. */
export default function globalTeardown(): void {
  stopHost();
}
