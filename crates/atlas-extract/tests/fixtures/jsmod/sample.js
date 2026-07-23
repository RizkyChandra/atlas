import { Response } from './models';

class HttpClient {
    constructor(baseUrl) {
        this.baseUrl = baseUrl;
    }

    get(path) {
        return fetch(this.baseUrl + path);
    }

    post(path, body) {
        return this.get(path);
    }
}

function buildHeaders(token) {
    return { Authorization: `Bearer ${token}` };
}

export { HttpClient, buildHeaders };
