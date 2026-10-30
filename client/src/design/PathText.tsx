// SPDX-License-Identifier: Apache-2.0
//
// A file-system path that wraps after its separators rather than mid-name.

import { Fragment } from "react";

export function PathText({ path }: { path: string }) {
  return (
    <>
      {path.split(/(?<=[/\\])/).map((part, i) => (
        <Fragment key={i}>
          {i > 0 && <wbr />}
          {part}
        </Fragment>
      ))}
    </>
  );
}
