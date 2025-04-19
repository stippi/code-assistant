Load files into working memory. You can specify line ranges by appending them to the file path using a colon.

Examples:
- file.txt - Read the entire file. Prefer this form unless you are absolutely sure you need only a section of the file.
- file.txt:10-20 - Read only lines 10 to 20
- file.txt:10- - Read from line 10 to the end
- file.txt:-20 - Read from the beginning to line 20
- file.txt:15 - Read only line 15
