# Problem reports

You can review a problem report before sending it. The computer keeps the
bundle exactly as you saw it, including any extras you chose.

The computer collects its versions and a short trail of states, error
codes, sandbox blocks, approvals, GitHub call results, alerts and updates.
The trail covers up to thirty minutes and 400 events. Task, run and approval
ids become different pseudonyms for each report. Panic records contain a
scrubbed message and source locations from Ophio's own code.

The trail never includes chats, prompts, code, file contents, paths, task or
project names, screenshots of the desktop, or credentials. A task transcript
and real names are extras, off by default. Each extra is shown before you
send it. Older transcript entries are left out when needed to fit.

Reports go to a private intake, never to public issues. The computer signs
requests with its own reporter key. If the intake cannot be reached, reports
wait on the computer and retry later. Taking back a report removes its local
bundle and queues its withdrawal; a summary stays in Your reports.

Report settings let you turn sending off and choose how early you hear about
fixes. With sending off, reports stay queued locally. Hearing about a release
does not download or install it. A halted build can show a way back when the
computer still has its previous build.

There is no intake URL by default. Set `intake_url = "https://intake.example"`
in the companion's `config.toml`, or set `OPHIO_INTAKE_URL` for development.
An empty environment value disables the intake. HTTP is allowed only on
localhost for development. Without an intake, reports stay on the computer.

The computer checks for status and questions soon after sending, then less
often, up to every four hours. Closed reports stop being checked. A question
appears in Needs you. Reports alert for a question, reproduction, or a fix;
routine review progress stays quiet. Phones must support report alerts to
receive them.

On the phone, open Report a problem in Settings, or tap Report on an error
notice or a refused action. After an unexpected close, Ophio offers to send
what happened. Not now clears that offer for those crashes. Phones that can
detect screenshots briefly offer Report a problem? after a screenshot.
Letting the offer fade away doesn't count against it. Closing it three times
in a row pauses it for a day.

The report opens over your current screen. On phones that support capturing
Ophio’s own view, a picture is attached before the sheet opens. Tap it to mark
it with pen, box, ellipse, arrow or pin, or remove it. The marks become part
of the picture. Other devices simply have no picture. Pictures show what
was on your screen, so check them before sending.

“What happened?” is optional. Type a sentence or use Speak, then edit the
words. See all shows every field in the redacted bundle that will be sent.
When a computer offers extras, switches let you include the task’s
conversation and real names for pseudonyms. Both start off. Reports carry a
still picture; Annotate’s project recordings and steps are separate.

Send saves the reviewed report on the phone and shows its six-character
reference at once. Delivery continues in the background. Without a reachable
computer, the phone can send directly to its configured private intake; when
neither route is available, the report waits on the phone.

Settings › Your reports combines the computer’s reports and this phone’s
own reports, newest first. Open one for its status history, questions and
See what was sent. Withdraw asks first, then removes the report from the
intake when it can be reached. Your reports also has Send reports and Hear
about fixes early. Intake connection problems appear there in a short line.

Questions offer buttons in Your reports and Needs you. Allowing a request
for more information opens a follow-up report to review before it is sent.
A fix in another build points to the usual phone update flow, with computer
update instructions when needed. Paused builds show instructions for going
back when a previous build is available.

Problem reports in notification settings controls report alerts. Tapping an
alert opens its report. Computers without report support receive no report
queries or commands; the phone’s direct reports remain available.
