# Problem

Oftentimes, we will have a git repository that has a commit history that is utter chaos. 
- Merge commits that aren't really necessary or don't add value to the history
- Poorly written commit messages/descriptions
- Commits that could be squashed but weren't (e.g, commits made to fix a fix that fixed a fix (maybe because there was a merge commit in the way and squashing wasn't possible)).
- Files that were accidentally committed and need to be removed from the history itself (e.g, sensitive or otherwise, it is junk)

Sometimes, I wish there was a way to go back in time and fix all my git mess-ups, and Git actually allows you to do this (rebase, filter-branch, etc.) but:
1. It's not user-friendly and can be time consuming, especially for those who are not command-line savvy.
2. It can be extremely risky if not done correctly

There are Git GUIs (Github Desktop, GitKraken, SourceTree, etc) but they don't really focus on the "cleaning up" aspect of git history or the problems I've mentioned above. 

## Solution

I am considering building a desktop app that lets you open a Git Repo, select a branch, visualize its commit history in a single linear list, and then:
1. Select multiple commits and squash them
2. Flatten a merge commit into a single commit (so the above squashing can be done even if there are merge commits in the way)
3. Edit commit messages/descriptions, or author information (this can be done across multiple commits at once)
4. Remove files from the history (and optionally add them to .gitignore)
5. Reorder commits

The UI should be minimal, intuitive, and user-friendly, making it easy for both beginners and experienced developers to clean up their Git history without the fear of breaking something.

It also works in a draft mode and only applies the changes to the actual Git history when the user confirms, allowing them to review their changes before making them permanent. 

Multiple selecting works by holding down the CTRL key and clicking on the commits.

We also need a search functionality to quickly find specific commits based on their messages, authors, or even file names. All the search results can be multi-selected and edited in bulk.