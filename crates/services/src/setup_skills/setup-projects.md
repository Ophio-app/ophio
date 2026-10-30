---
name: Add my projects
description: Find the folders the person works in and add the ones they choose as Øphio projects.
---
Help the person add the folders they work in as Øphio projects, so agents can work in them from the phone.

1. Run `ophio projects list` to see what is there already.
2. Look for likely project folders without opening their files: folders holding a `.git` folder under the home folder's usual places, such as `~/Projects`, `~/code`, `~/src`, `~/dev` and `~/Documents`. Look at most three levels deep and skip hidden folders, `node_modules`, caches and anything inside Øphio's own folders.
3. Show the person what you found, as a short numbered list with each folder's path, and ask which to add. Add only the ones they choose.
4. Add each with `ophio projects add <folder>`, or `ophio projects add <folder> --name "<name>"` for a name they'd rather see.
5. Run `ophio projects list` again and show the result.

Don't change anything inside the folders.
