
Bootstrap uses the bounded task-history snapshot and on-demand run reads. A failed snapshot now leaves a visible connection error and retries on the same transport. Snapshot replies are fenced by connection generation, including refreshes after a journal gap; interrupted loads never replace the current projection.
