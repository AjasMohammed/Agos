// Alpine.js component for the pipeline list page.
// csrfToken is passed as a parameter from the x-data attribute in the template.
function pipelineList(csrfToken) {
    return {
        showRun: false,
        selectedPipeline: '',
        csrfToken: csrfToken,
        goToEdit(name) {
            window.location.href = '/pipelines/' + encodeURIComponent(name) + '/edit';
        },
        async postAction(url) {
            var response = await fetch(url, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8',
                    'X-CSRF-Token': this.csrfToken
                },
                body: new URLSearchParams({ _csrf: this.csrfToken }).toString()
            });
            if (!response.ok) {
                var message = await response.text();
                window.dispatchEvent(new CustomEvent('show-toast', { detail: { message: message || 'Pipeline action failed', type: 'error' } }));
                return;
            }
            window.location.reload();
        },
        clonePipeline(name) {
            this.postAction('/pipelines/' + encodeURIComponent(name) + '/clone');
        },
        deletePipeline(name) {
            if (!window.confirm('Delete pipeline ' + name + '?')) return;
            this.postAction('/pipelines/' + encodeURIComponent(name) + '/delete');
        }
    };
}
